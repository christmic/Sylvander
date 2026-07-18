//! Runtime-owned supervision for Worker capabilities and Guardian curation.
//!
//! This module is the composition boundary missing from the lower-level
//! capability and curation stores. It owns expiring Guardian credentials,
//! immutable actor snapshots, leased curation work, and an idempotent
//! canonical-memory sink. No channel, model response, or tool input can issue
//! a Guardian identity or choose an owner.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sylvander_agent::curated_memory::{
    CuratedContextEntry, CuratedContextProvider, CuratedContextSubject, CuratedMemoryScope,
    MemoryCandidateError, MemoryCandidateReceipt, MemoryCandidateSink, MemoryCandidateSubmission,
};
use sylvander_agent::session_store::StoredSession;
use sylvander_agent::tool_context::ToolContext;
use sylvander_agent::tool_invocation::{
    AuthorizedToolInvocation, CapabilityFeature, CapabilityFeatureKind, ToolInvocationClass,
    ToolInvocationDescriptor, ToolInvocationError, ToolInvocationGateway, ToolInvocationOutcome,
    ToolInvocationRequest, ToolInvocationSnapshot,
};
use sylvander_agent::tools::MemoryOwner;
use sylvander_protocol::{SessionContext, UserId};
use thiserror::Error;
use tokio::sync::{RwLock, watch};
use tokio::task::JoinHandle;

use crate::capability_runtime::{
    ActorCapabilityRuntime, ActorCapabilitySnapshot, AuthorizedCapabilityInvocation,
    CapabilityActor, CapabilityAuditSink, CapabilityClass, CapabilityDefinition,
    CapabilityRegistry, CapabilityRuntimeError, GuardianServiceIdentity, RuntimeCapability,
    RuntimeOwnerScope, value_digest,
};
use crate::guardian_curation::{
    CandidateClassification, CandidateDraft, CandidateOrigin, CandidateScope, CandidateState,
    ClaimedCuratorRun, ClaimedMutation, CuratorRunState, EvidenceReference, GuardianCurationError,
    GuardianCurationStore, GuardianEvent, GuardianEventKind, MutationAction, MutationDeliveryState,
    PolicyOutcome, Reconciliation, Sensitivity,
};
use crate::user_profile_store::UserProfileStore;

const CANONICAL_APPLICATION_ID: i64 = 1_398_361_987;
const CANONICAL_SCHEMA_VERSION: i64 = 2;
const BUILTIN_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;
const MAX_RUNS_PER_PASS: usize = 32;
const MAX_MUTATIONS_PER_PASS: usize = 64;
const MAX_CANDIDATE_CONTENT_BYTES: usize = 16 * 1024;
const MAX_CANDIDATE_TAGS: usize = 32;
const MAX_CANDIDATE_TAG_BYTES: usize = 64;

/// Latest-only settings for one Runtime-owned Guardian service.
#[derive(Debug, Clone)]
pub(crate) struct GuardianRuntimeSettings {
    pub(crate) curation_path: PathBuf,
    pub(crate) canonical_path: PathBuf,
    pub(crate) service_id: String,
    pub(crate) initial_credential_revision: u64,
    pub(crate) policy_revision: u64,
    pub(crate) curator_version: String,
    pub(crate) identity_ttl_secs: i64,
    pub(crate) lease_secs: i64,
    pub(crate) retry_delay_secs: i64,
    pub(crate) max_attempts: u32,
    pub(crate) poll_interval: Duration,
}

impl GuardianRuntimeSettings {
    pub(crate) fn for_runtime(data_dir: &Path, runtime_name: &str, now: i64) -> Self {
        Self {
            curation_path: data_dir.join("guardian-curation.db"),
            canonical_path: data_dir.join("guardian-canonical.db"),
            service_id: domain_digest(b"sylvander.guardian.runtime-service.v1\0", runtime_name),
            initial_credential_revision: u64::try_from(now).unwrap_or(1).max(1),
            policy_revision: 1,
            curator_version: "builtin-reference-v1".into(),
            identity_ttl_secs: 15 * 60,
            lease_secs: 30,
            retry_delay_secs: 5,
            max_attempts: 5,
            poll_interval: Duration::from_millis(250),
        }
    }

    fn validate(&self) -> Result<(), GuardianRuntimeError> {
        if self.service_id.trim().is_empty()
            || self.curator_version.trim().is_empty()
            || self.initial_credential_revision == 0
            || self.policy_revision == 0
            || self.lease_secs <= 0
            || self.retry_delay_secs <= 0
            || self.max_attempts == 0
            || self.poll_interval.is_zero()
            || self.identity_ttl_secs <= self.lease_secs.saturating_mul(2)
        {
            return Err(GuardianRuntimeError::InvalidConfiguration);
        }
        Ok(())
    }
}

struct ActorMetadataCapability {
    actor: CapabilityActor,
}

struct ToolAuthorizationCapability {
    definition: CapabilityDefinition,
}

#[async_trait]
pub(crate) trait LearningPreferenceSource: Send + Sync {
    async fn do_not_learn(&self, owner: &UserId) -> Result<bool, ()>;
}

#[async_trait]
impl LearningPreferenceSource for UserProfileStore {
    async fn do_not_learn(&self, owner: &UserId) -> Result<bool, ()> {
        self.read(owner.clone())
            .await
            .map(|profile| profile.is_some_and(|profile| profile.do_not_learn))
            .map_err(|_| ())
    }
}

#[async_trait]
impl RuntimeCapability for ToolAuthorizationCapability {
    fn definition(&self) -> CapabilityDefinition {
        self.definition.clone()
    }

    async fn invoke(
        &self,
        _invocation: AuthorizedCapabilityInvocation<'_>,
    ) -> Result<Value, CapabilityRuntimeError> {
        // Executable adapters run in sylvander-agent only after
        // `authorize_external`; invoking this policy entry is always a bug.
        Err(CapabilityRuntimeError::ExecutionFailed)
    }
}

#[async_trait]
impl RuntimeCapability for ActorMetadataCapability {
    fn definition(&self) -> CapabilityDefinition {
        let schema = json!({"type": "object", "additionalProperties": false});
        CapabilityDefinition {
            name: match self.actor {
                CapabilityActor::Worker => "worker.runtime_metadata",
                CapabilityActor::Guardian => "guardian.runtime_metadata",
            }
            .into(),
            version: 1,
            class: CapabilityClass::Read,
            schema_digest: value_digest(&schema),
            schema,
        }
    }

    async fn invoke(
        &self,
        invocation: AuthorizedCapabilityInvocation<'_>,
    ) -> Result<Value, CapabilityRuntimeError> {
        if !invocation
            .input
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
            || invocation.actor != self.actor
            || invocation.invocation_id.is_empty()
        {
            return Err(CapabilityRuntimeError::ExecutionFailed);
        }
        Ok(json!({
            "actor": match self.actor {
                CapabilityActor::Worker => "worker",
                CapabilityActor::Guardian => "guardian",
            },
            "user_bound": invocation.owner.user_id.is_some(),
            "session_bound": invocation.owner.session_id.is_some(),
            "workspace_count": invocation.owner.workspace_ids.len(),
        }))
    }
}

struct GuardianEpoch {
    credential_revision: u64,
    identity: GuardianServiceIdentity,
    store: GuardianCurationStore,
    capabilities: ActorCapabilityRuntime,
}

impl GuardianEpoch {
    async fn open(
        settings: &GuardianRuntimeSettings,
        credential_revision: u64,
        now: i64,
    ) -> Result<Self, GuardianRuntimeError> {
        let expires_at = now
            .checked_add(settings.identity_ttl_secs)
            .ok_or(GuardianRuntimeError::InvalidConfiguration)?;
        let identity = GuardianServiceIdentity::issue(
            settings.service_id.clone(),
            credential_revision,
            expires_at,
        )?;
        let store = GuardianCurationStore::open(
            &settings.curation_path,
            identity.clone(),
            settings.policy_revision,
        )
        .await?;
        let worker = CapabilityRegistry::new().register(ActorMetadataCapability {
            actor: CapabilityActor::Worker,
        })?;
        let guardian = CapabilityRegistry::new().register(ActorMetadataCapability {
            actor: CapabilityActor::Guardian,
        })?;
        let capabilities = ActorCapabilityRuntime::new(
            worker,
            guardian,
            identity.clone(),
            settings.policy_revision,
            Arc::new(store.clone()),
        )?;
        Ok(Self {
            credential_revision,
            identity,
            store,
            capabilities,
        })
    }
}

/// Runtime service that supervises durable curation until graceful shutdown.
pub(crate) struct GuardianRuntime {
    inner: Arc<GuardianRuntimeInner>,
    shutdown: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

/// Cloneable composition handle used for both boot-time and lazily rebuilt
/// Agent revisions.
#[derive(Clone)]
pub(crate) struct WorkerToolGatewayFactory {
    audit: GuardianCurationStore,
    canonical: GuardianCanonicalStore,
    guardian_identity: GuardianServiceIdentity,
    policy_revision: u64,
    learning_preferences: Arc<dyn LearningPreferenceSource>,
}

impl WorkerToolGatewayFactory {
    pub(crate) async fn open(
        settings: &GuardianRuntimeSettings,
        now: i64,
        user_profiles: UserProfileStore,
    ) -> Result<Self, GuardianRuntimeError> {
        settings.validate()?;
        create_parent(&settings.curation_path)?;
        create_parent(&settings.canonical_path)?;
        let guardian_identity = GuardianServiceIdentity::issue(
            settings.service_id.clone(),
            settings.initial_credential_revision,
            now.checked_add(settings.identity_ttl_secs)
                .ok_or(GuardianRuntimeError::InvalidConfiguration)?,
        )?;
        let audit = GuardianCurationStore::open(
            &settings.curation_path,
            guardian_identity.clone(),
            settings.policy_revision,
        )
        .await?;
        let canonical = GuardianCanonicalStore::open(&settings.canonical_path).await?;
        Ok(Self {
            audit,
            canonical,
            guardian_identity,
            policy_revision: settings.policy_revision,
            learning_preferences: Arc::new(user_profiles),
        })
    }

    pub(crate) fn build(
        &self,
        agent_id: sylvander_protocol::AgentId,
        descriptors: Vec<ToolInvocationDescriptor>,
    ) -> Result<Arc<dyn ToolInvocationGateway>, GuardianRuntimeError> {
        build_worker_tool_gateway(
            agent_id,
            descriptors,
            self.learning_preferences.clone(),
            self.guardian_identity.clone(),
            self.policy_revision,
            Arc::new(self.audit.clone()),
        )
    }

    pub(crate) fn candidate_sink(&self) -> Arc<dyn MemoryCandidateSink> {
        Arc::new(GuardianCandidateGateway {
            curation: self.audit.clone(),
            canonical: self.canonical.clone(),
            learning_preferences: self.learning_preferences.clone(),
        })
    }

    pub(crate) fn curated_context_provider(&self) -> Arc<dyn CuratedContextProvider> {
        Arc::new(self.canonical.clone())
    }
}

struct GuardianRuntimeInner {
    settings: GuardianRuntimeSettings,
    epoch: RwLock<GuardianEpoch>,
    canonical: GuardianCanonicalStore,
    learning_preferences: Arc<dyn LearningPreferenceSource>,
    last_error: RwLock<Option<String>>,
}

impl GuardianRuntime {
    /// Open latest-schema stores, issue a short-lived identity, and start the
    /// single-owner supervisor. Reopening the same paths resumes expired work.
    pub(crate) async fn start(
        settings: GuardianRuntimeSettings,
        now: i64,
        learning_preferences: Arc<dyn LearningPreferenceSource>,
    ) -> Result<Self, GuardianRuntimeError> {
        settings.validate()?;
        create_parent(&settings.curation_path)?;
        create_parent(&settings.canonical_path)?;
        let epoch =
            GuardianEpoch::open(&settings, settings.initial_credential_revision, now).await?;
        let canonical = GuardianCanonicalStore::open(&settings.canonical_path).await?;
        let inner = Arc::new(GuardianRuntimeInner {
            settings,
            epoch: RwLock::new(epoch),
            canonical,
            learning_preferences,
            last_error: RwLock::new(None),
        });
        let (shutdown, receiver) = watch::channel(false);
        let worker = inner.clone();
        let task = tokio::spawn(async move { worker.supervise(receiver).await });
        Ok(Self {
            inner,
            shutdown,
            task: Mutex::new(Some(task)),
        })
    }

    pub(crate) async fn enqueue_event(
        &self,
        event: GuardianEvent,
        available_at_unix_secs: i64,
    ) -> Result<bool, GuardianRuntimeError> {
        let learning_input = matches!(
            event.kind,
            GuardianEventKind::MemoryCandidateCreated
                | GuardianEventKind::SessionClosed
                | GuardianEventKind::UserFeedbackReceived
        );
        if learning_input
            && !learning_source_allowed(
                self.inner.learning_preferences.as_ref(),
                event.owner.user_id.as_ref(),
            )
            .await?
        {
            return Ok(false);
        }
        self.inner
            .epoch
            .read()
            .await
            .store
            .enqueue_event(event, available_at_unix_secs)
            .await
            .map_err(Into::into)
    }

    /// Enqueue an immutable reference after a persisted feedback record exists.
    pub(crate) async fn enqueue_feedback(
        &self,
        session: &StoredSession,
        feedback_id: &str,
        payload_digest: &str,
        occurred_at_unix_secs: i64,
    ) -> Result<bool, GuardianRuntimeError> {
        let event = GuardianEvent::new(
            format!("feedback:{feedback_id}"),
            GuardianEventKind::UserFeedbackReceived,
            owner_from_session(session)?,
            vec![EvidenceReference {
                kind: "evidence_feedback".into(),
                reference: feedback_id.into(),
                digest: payload_digest.into(),
            }],
            payload_digest,
            occurred_at_unix_secs,
        );
        self.enqueue_event(event, occurred_at_unix_secs).await
    }

    /// Enqueue session closure without copying session history or workspace
    /// paths into the curation database.
    pub(crate) async fn enqueue_session_closed(
        &self,
        session: &StoredSession,
        occurred_at_unix_secs: i64,
    ) -> Result<bool, GuardianRuntimeError> {
        let reference = format!("{}:{}", session.id.0, session.updated_at);
        let payload_digest = domain_digest(b"sylvander.guardian.session-closed.v1\0", &reference);
        let event = GuardianEvent::new(
            format!("session-closed:{reference}"),
            GuardianEventKind::SessionClosed,
            owner_from_session(session)?,
            vec![EvidenceReference {
                kind: "session".into(),
                reference: session.id.0.clone(),
                digest: payload_digest.clone(),
            }],
            payload_digest,
            occurred_at_unix_secs,
        );
        let inserted = self.enqueue_event(event, occurred_at_unix_secs).await?;
        // Session closure is the natural bounded cadence for owner-scoped
        // non-workspace retention. Workspace schedulers call the same typed
        // entry with Runtime-derived execution-target identifiers.
        let _ = self
            .enqueue_retention_sweep(session, BTreeSet::new(), occurred_at_unix_secs)
            .await?;
        Ok(inserted)
    }

    /// Persist an authenticated user decision as a distinct immutable event.
    ///
    /// The candidate supplies the governed workspace scope; callers cannot
    /// broaden it through request data. The session must own the candidate.
    pub(crate) async fn enqueue_confirmation(
        &self,
        session: &StoredSession,
        candidate_id: &str,
        expected_revision: u64,
        confirmed: bool,
        occurred_at_unix_secs: i64,
    ) -> Result<bool, GuardianRuntimeError> {
        if candidate_id.trim().is_empty() || expected_revision == 0 {
            return Err(GuardianRuntimeError::InvalidConfiguration);
        }
        let session_owner = owner_from_session(session)?;
        let candidate = self
            .inner
            .epoch
            .read()
            .await
            .store
            .candidate(candidate_id)
            .await?;
        if candidate.owner_agent_id != session_owner.agent_id
            || candidate.owner_user_id != session_owner.user_id
        {
            return Err(GuardianCurationError::AccessDenied.into());
        }
        let payload = self
            .inner
            .canonical
            .payload_for_candidate(&candidate)
            .await?;
        if payload.origin_session_id != session.id.0 {
            return Err(GuardianCurationError::AccessDenied.into());
        }
        if candidate.revision != expected_revision
            || candidate.state != CandidateState::AwaitingConfirmation
        {
            return Err(GuardianCurationError::Conflict.into());
        }
        let owner = RuntimeOwnerScope::guardian(
            candidate.owner_agent_id,
            candidate.owner_user_id,
            candidate.workspace_id.into_iter().collect(),
        );
        let confirmation = StagedConfirmation {
            candidate_id: candidate_id.to_owned(),
            expected_revision,
            confirmed,
        };
        let value = serde_json::to_value(&confirmation)
            .map_err(|_| GuardianRuntimeError::InvalidConfiguration)?;
        let digest = digest_value(b"sylvander.guardian.confirmation.v1\0", &value)
            .map_err(|_| GuardianRuntimeError::InvalidConfiguration)?;
        let confirmation_id = domain_digest(
            b"sylvander.guardian.confirmation-id.v1\0",
            &format!(
                "{}\0{}\0{}\0{}",
                candidate_id, expected_revision, confirmed, session.id.0
            ),
        );
        let staged_at = self
            .inner
            .canonical
            .stage_confirmation(
                &confirmation_id,
                &owner,
                &confirmation,
                &digest,
                occurred_at_unix_secs,
            )
            .await?;
        let event = GuardianEvent::new(
            format!("confirmation:{confirmation_id}"),
            GuardianEventKind::UserConfirmationReceived,
            owner,
            vec![EvidenceReference {
                kind: "guardian_confirmation".into(),
                reference: confirmation_id,
                digest: digest.clone(),
            }],
            digest,
            staged_at,
        );
        self.enqueue_event(event, occurred_at_unix_secs).await
    }

    /// Return all pending confirmations that originated in this exact owned
    /// session. Draining first removes the race between a completed Agent turn
    /// and the Guardian background poll.
    pub(crate) async fn pending_confirmations(
        &self,
        session: &StoredSession,
        now: i64,
    ) -> Result<Vec<sylvander_protocol::PendingMemoryConfirmation>, GuardianRuntimeError> {
        for _ in 0..MAX_RUNS_PER_PASS {
            if !self.drain_once(now).await? {
                break;
            }
        }
        let owner = owner_from_session(session)?;
        let user_id = owner
            .user_id
            .clone()
            .ok_or(GuardianRuntimeError::InvalidConfiguration)?;
        let candidates = self
            .inner
            .epoch
            .read()
            .await
            .store
            .pending_confirmations(owner.agent_id, user_id)
            .await?;
        let mut pending = Vec::new();
        for candidate in candidates {
            let payload = self
                .inner
                .canonical
                .payload_for_candidate(&candidate)
                .await?;
            if payload.origin_session_id != session.id.0 {
                continue;
            }
            let scope = match candidate
                .scope
                .ok_or(GuardianRuntimeError::InvalidConfiguration)?
            {
                CandidateScope::Relationship => {
                    sylvander_protocol::MemoryConfirmationScope::Relationship
                }
                CandidateScope::UserProfile => {
                    sylvander_protocol::MemoryConfirmationScope::UserProfile
                }
                CandidateScope::AgentCanonical => {
                    sylvander_protocol::MemoryConfirmationScope::AgentCanonical
                }
                CandidateScope::WorkspaceKnowledge => {
                    sylvander_protocol::MemoryConfirmationScope::WorkspaceKnowledge
                }
            };
            pending.push(sylvander_protocol::PendingMemoryConfirmation {
                candidate_id: candidate.candidate_id,
                expected_revision: candidate.revision,
                scope,
                summary: if candidate.sensitivity == Some(Sensitivity::Secret) {
                    "Sensitive material was detected and cannot be stored".into()
                } else {
                    confirmation_summary(&candidate.content)
                },
            });
        }
        Ok(pending)
    }

    /// Record one explicit decision and synchronously consume its immutable
    /// confirmation event. Replays and stale revisions fail closed.
    pub(crate) async fn resolve_confirmation(
        &self,
        session: &StoredSession,
        candidate_id: &str,
        expected_revision: u64,
        decision: sylvander_protocol::MemoryConfirmationDecision,
        now: i64,
    ) -> Result<(), GuardianRuntimeError> {
        let inserted = self
            .enqueue_confirmation(
                session,
                candidate_id,
                expected_revision,
                matches!(
                    decision,
                    sylvander_protocol::MemoryConfirmationDecision::Confirm
                ),
                now,
            )
            .await?;
        if !inserted {
            return Err(GuardianCurationError::Conflict.into());
        }
        for _ in 0..MAX_RUNS_PER_PASS {
            if !self.drain_once(now).await? {
                break;
            }
            let candidate = self
                .inner
                .epoch
                .read()
                .await
                .store
                .candidate(candidate_id)
                .await?;
            if candidate.state != CandidateState::AwaitingConfirmation {
                return Ok(());
            }
        }
        Err(GuardianCurationError::Conflict.into())
    }

    /// Enqueue an owner-scoped retention pass. Workspace identifiers must
    /// come from Runtime bindings, never model or channel payloads.
    pub(crate) async fn enqueue_retention_sweep(
        &self,
        session: &StoredSession,
        workspace_ids: BTreeSet<String>,
        occurred_at_unix_secs: i64,
    ) -> Result<bool, GuardianRuntimeError> {
        if workspace_ids.iter().any(|id| id.trim().is_empty()) {
            return Err(GuardianRuntimeError::InvalidConfiguration);
        }
        let mut owner = owner_from_session(session)?;
        owner.workspace_ids = workspace_ids;
        let reference = format!(
            "{}:{}:{}",
            owner.agent_id.0, session.metadata.user_id, occurred_at_unix_secs
        );
        let digest = domain_digest(b"sylvander.guardian.retention-sweep.v1\0", &reference);
        let event = GuardianEvent::new(
            format!("retention:{reference}"),
            GuardianEventKind::RetentionSweep,
            owner,
            vec![EvidenceReference {
                kind: "guardian_retention_sweep".into(),
                reference,
                digest: digest.clone(),
            }],
            digest,
            occurred_at_unix_secs,
        );
        self.enqueue_event(event, occurred_at_unix_secs).await
    }

    /// Freeze a Worker view from an authenticated Runtime session.
    pub(crate) async fn worker_snapshot(
        &self,
        session: &SessionContext,
        workspace_ids: BTreeSet<String>,
    ) -> ActorCapabilitySnapshot {
        self.inner
            .epoch
            .read()
            .await
            .capabilities
            .begin_worker_run(session, workspace_ids)
    }
}

fn build_worker_tool_gateway(
    agent_id: sylvander_protocol::AgentId,
    descriptors: Vec<ToolInvocationDescriptor>,
    learning_preferences: Arc<dyn LearningPreferenceSource>,
    guardian_identity: GuardianServiceIdentity,
    policy_revision: u64,
    audit: Arc<dyn CapabilityAuditSink>,
) -> Result<Arc<dyn ToolInvocationGateway>, GuardianRuntimeError> {
    let mut worker = CapabilityRegistry::new().register(ActorMetadataCapability {
        actor: CapabilityActor::Worker,
    })?;
    for descriptor in &descriptors {
        let schema = descriptor.input_schema.clone();
        worker = worker.register(ToolAuthorizationCapability {
            definition: CapabilityDefinition {
                name: tool_capability_name(&descriptor.name),
                version: 1,
                class: runtime_tool_class(descriptor.class),
                schema_digest: value_digest(&schema),
                schema,
            },
        })?;
    }
    let guardian = CapabilityRegistry::new().register(ActorMetadataCapability {
        actor: CapabilityActor::Guardian,
    })?;
    let capabilities = Arc::new(ActorCapabilityRuntime::new(
        worker,
        guardian,
        guardian_identity,
        policy_revision,
        audit,
    )?);
    Ok(Arc::new(RuntimeWorkerToolGateway {
        capabilities,
        agent_id,
        routes: descriptors
            .iter()
            .map(|descriptor| (descriptor.name.clone(), descriptor.class))
            .collect(),
        snapshot: ToolInvocationSnapshot::from_descriptors(&descriptors),
        learning_preferences,
    }))
}

impl GuardianRuntime {
    /// Re-authorize and durably audit the Worker capability boundary when a
    /// persisted session is attached to an Agent run.
    pub(crate) async fn audit_worker_session_binding(
        &self,
        session: &StoredSession,
        now: i64,
    ) -> Result<(), GuardianRuntimeError> {
        let owner = owner_from_session(session)?;
        let context = SessionContext::new(
            session.metadata.user_id.clone(),
            owner.agent_id.0,
            session.id.clone(),
        );
        let snapshot = self.worker_snapshot(&context, BTreeSet::new()).await;
        if snapshot.actor() != CapabilityActor::Worker
            || snapshot.definitions().len() != 1
            || snapshot.revision().is_empty()
        {
            return Err(GuardianRuntimeError::InvalidConfiguration);
        }
        snapshot
            .invoke("worker.runtime_metadata", &json!({}), now)
            .await?;
        Ok(())
    }

    /// Run one deterministic supervision pass; primarily used by boot catch-up
    /// and focused recovery tests.
    pub(crate) async fn drain_once(&self, now: i64) -> Result<bool, GuardianRuntimeError> {
        self.inner.drain_once(now).await
    }

    pub(crate) async fn last_error(&self) -> Option<String> {
        self.inner.last_error.read().await.clone()
    }

    #[cfg(test)]
    pub(crate) async fn set_last_error_for_test(&self, failed: bool) {
        *self.inner.last_error.write().await =
            failed.then(|| "private injected Guardian failure".into());
    }

    #[cfg(test)]
    pub(crate) fn canonical_record_count(&self) -> i64 {
        let connection = self.inner.canonical.connection.lock().unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM guardian_canonical_memory WHERE deleted=0",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[cfg(test)]
    pub(crate) fn completed_event_count(&self) -> i64 {
        Connection::open(&self.inner.settings.curation_path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM guardian_outbox WHERE state='completed'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// Stop polling and await the active pass. `SQLite` transactions are
    /// synchronous and finish before the task observes shutdown.
    pub(crate) async fn shutdown(&self) -> Result<(), GuardianRuntimeError> {
        let _ = self.shutdown.send(true);
        let task = self
            .task
            .lock()
            .map_err(|_| GuardianRuntimeError::Supervisor)?
            .take();
        if let Some(task) = task {
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .map_err(|_| GuardianRuntimeError::Supervisor)?
                .map_err(|_| GuardianRuntimeError::Supervisor)?;
        }
        Ok(())
    }
}

struct RuntimeWorkerToolGateway {
    capabilities: Arc<ActorCapabilityRuntime>,
    agent_id: sylvander_protocol::AgentId,
    routes: BTreeMap<String, ToolInvocationClass>,
    snapshot: ToolInvocationSnapshot,
    learning_preferences: Arc<dyn LearningPreferenceSource>,
}

struct RuntimeWorkerToolGrant {
    lease: Option<crate::capability_runtime::ExternalCapabilityInvocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagedCandidatePayload {
    origin_session_id: String,
    source_key: String,
    content: Value,
    evidence: Vec<EvidenceReference>,
    origin: CandidateOrigin,
    classification: StagedCandidateClassification,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagedCandidateClassification {
    scope: CandidateScope,
    confidence_basis_points: u16,
    sensitivity: Sensitivity,
    retention_secs: u64,
    dedupe_key: String,
    workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagedConfirmation {
    candidate_id: String,
    expected_revision: u64,
    confirmed: bool,
}

#[derive(Clone)]
struct GuardianCandidateGateway {
    curation: GuardianCurationStore,
    canonical: GuardianCanonicalStore,
    learning_preferences: Arc<dyn LearningPreferenceSource>,
}

#[async_trait]
impl MemoryCandidateSink for GuardianCandidateGateway {
    async fn submit(
        &self,
        context: &ToolContext,
        candidate: MemoryCandidateSubmission,
    ) -> Result<MemoryCandidateReceipt, MemoryCandidateError> {
        if candidate.content.trim().is_empty()
            || candidate.content.len() > MAX_CANDIDATE_CONTENT_BYTES
            || candidate.tags.len() > MAX_CANDIDATE_TAGS
            || candidate
                .tags
                .iter()
                .any(|tag| tag.trim().is_empty() || tag.len() > MAX_CANDIDATE_TAG_BYTES)
        {
            return Err(MemoryCandidateError::Invalid);
        }
        let MemoryOwner::Relationship { user_id, agent_id } = context
            .memory_context()
            .relationship_owner()
            .map_err(|_| MemoryCandidateError::AccessDenied)?
        else {
            return Err(MemoryCandidateError::AccessDenied);
        };
        if self
            .learning_preferences
            .do_not_learn(&user_id)
            .await
            .map_err(|()| MemoryCandidateError::Unavailable)?
        {
            return Err(MemoryCandidateError::AccessDenied);
        }
        if context.execution_target.id.trim().is_empty() {
            return Err(MemoryCandidateError::Invalid);
        }
        let (scope, sensitivity, workspace_id) = match candidate.scope {
            CuratedMemoryScope::Relationship => {
                (CandidateScope::Relationship, Sensitivity::Internal, None)
            }
            CuratedMemoryScope::UserProfile => {
                (CandidateScope::UserProfile, Sensitivity::Personal, None)
            }
            CuratedMemoryScope::AgentCanonical => {
                (CandidateScope::AgentCanonical, Sensitivity::Internal, None)
            }
            CuratedMemoryScope::WorkspaceKnowledge => (
                CandidateScope::WorkspaceKnowledge,
                Sensitivity::Internal,
                Some(context.execution_target.id.clone()),
            ),
        };
        let candidate_body = json!({"text": candidate.content, "tags": candidate.tags});
        let governed_payload = json!({
            "content": &candidate_body,
            "scope": scope,
            "sensitivity": sensitivity,
            "workspace_id": workspace_id,
        });
        let payload_digest = digest_value(
            b"sylvander.guardian.learning-payload.v1\0",
            &governed_payload,
        )
        .map_err(|_| MemoryCandidateError::Invalid)?;
        let source_key = domain_digest(
            b"sylvander.guardian.worker-candidate.v1\0",
            &format!(
                "{}\0{}\0{}\0{}\0{}",
                user_id.0,
                agent_id.0,
                context.session_id().0,
                context.session.request.created_at,
                payload_digest
            ),
        );
        let intake_id = source_key.clone();
        let evidence = vec![EvidenceReference {
            kind: "worker_memory_candidate".into(),
            reference: intake_id.clone(),
            digest: payload_digest.clone(),
        }];
        let staged = StagedCandidatePayload {
            origin_session_id: context.session_id().0.clone(),
            source_key,
            content: candidate_body,
            evidence: evidence.clone(),
            origin: CandidateOrigin::Explicit,
            classification: StagedCandidateClassification {
                scope,
                confidence_basis_points: 10_000,
                sensitivity,
                retention_secs: BUILTIN_RETENTION_SECS,
                dedupe_key: payload_digest.clone(),
                workspace_id: workspace_id.clone(),
            },
        };
        let owner = RuntimeOwnerScope::guardian(
            agent_id,
            Some(user_id),
            workspace_id.into_iter().collect(),
        );
        let available_at = now_seconds();
        let occurred_at = context.session.request.created_at;
        self.canonical
            .stage_payload(&intake_id, &owner, &staged, &payload_digest, available_at)
            .await
            .map_err(|_| MemoryCandidateError::Unavailable)?;
        let event_id = format!("memory-candidate:{intake_id}");
        let event = GuardianEvent::new(
            &event_id,
            GuardianEventKind::MemoryCandidateCreated,
            owner,
            evidence,
            payload_digest,
            occurred_at,
        );
        self.curation
            .enqueue_event(event, available_at)
            .await
            .map_err(|_| MemoryCandidateError::Unavailable)?;
        Ok(MemoryCandidateReceipt { event_id })
    }
}

#[async_trait]
impl AuthorizedToolInvocation for RuntimeWorkerToolGrant {
    async fn finish(
        mut self: Box<Self>,
        outcome: ToolInvocationOutcome,
    ) -> Result<(), ToolInvocationError> {
        self.lease
            .take()
            .ok_or(ToolInvocationError::ExecutionOutcomeUncertain)?
            .finish(matches!(outcome, ToolInvocationOutcome::Succeeded))
            .map_err(map_invocation_error)
    }
}

#[async_trait]
impl ToolInvocationGateway for RuntimeWorkerToolGateway {
    fn snapshot(&self) -> ToolInvocationSnapshot {
        self.snapshot.clone()
    }

    async fn authorize(
        &self,
        request: ToolInvocationRequest,
    ) -> Result<Box<dyn AuthorizedToolInvocation>, ToolInvocationError> {
        let class = request.class().ok_or(ToolInvocationError::Unavailable)?;
        if self.routes.get(request.route()) != Some(&class)
            || request.context().agent_id() != &self.agent_id
            || request.call_id().is_empty()
            || !self
                .snapshot
                .has_same_executable_surface(request.snapshot())
            || !request.snapshot().features().contains(&CapabilityFeature {
                name: request.route().to_owned(),
                kind: CapabilityFeatureKind::Executable(class),
            })
        {
            return Err(ToolInvocationError::AccessDenied);
        }
        let identity = &request.context().session.identity;
        if identity.user_id.0.is_empty() || identity.session_id.0.is_empty() {
            return Err(ToolInvocationError::AccessDenied);
        }
        if class == ToolInvocationClass::MemoryCandidate {
            let blocked = self
                .learning_preferences
                .do_not_learn(&identity.user_id)
                .await
                .map_err(|()| ToolInvocationError::AccessDenied)?;
            if blocked {
                return Err(ToolInvocationError::AccessDenied);
            }
        }

        let workspace_ids = BTreeSet::from([request.context().execution_target.id.clone()]);
        let snapshot = self
            .capabilities
            .begin_worker_run(request.context().session.as_ref(), workspace_ids);
        let lease = snapshot
            .authorize_external(
                &tool_capability_name(request.route()),
                request.input(),
                request.snapshot().revision(),
                sylvander_agent::session::now_secs(),
            )
            .map_err(map_invocation_error)?;
        Ok(Box::new(RuntimeWorkerToolGrant { lease: Some(lease) }))
    }
}

fn runtime_tool_class(class: ToolInvocationClass) -> CapabilityClass {
    match class {
        ToolInvocationClass::Read => CapabilityClass::Read,
        ToolInvocationClass::FilesystemMutation => CapabilityClass::WorkspaceMutation,
        ToolInvocationClass::Terminal => CapabilityClass::Terminal,
        ToolInvocationClass::Browser => CapabilityClass::Browser,
        ToolInvocationClass::HostControl => CapabilityClass::HostControl,
        ToolInvocationClass::ArbitraryMcp => CapabilityClass::ArbitraryMcp,
        ToolInvocationClass::MemoryCandidate => CapabilityClass::AgentCandidateAppend,
        ToolInvocationClass::Control | ToolInvocationClass::Extension => CapabilityClass::Extension,
    }
}

fn tool_capability_name(route: &str) -> String {
    format!("worker.tool::{route}")
}

fn map_invocation_error(error: CapabilityRuntimeError) -> ToolInvocationError {
    match error {
        CapabilityRuntimeError::CapabilityUnavailable => ToolInvocationError::Unavailable,
        CapabilityRuntimeError::AccessDenied
        | CapabilityRuntimeError::InvalidConfiguration
        | CapabilityRuntimeError::ExecutionFailed => ToolInvocationError::AccessDenied,
        CapabilityRuntimeError::AuditUnavailable => ToolInvocationError::AuditUnavailable,
        CapabilityRuntimeError::ExecutionOutcomeUncertain => {
            ToolInvocationError::ExecutionOutcomeUncertain
        }
    }
}

async fn learning_source_allowed(
    preferences: &dyn LearningPreferenceSource,
    owner: Option<&UserId>,
) -> Result<bool, GuardianRuntimeError> {
    let Some(owner) = owner else {
        return Ok(true);
    };
    preferences
        .do_not_learn(owner)
        .await
        .map(|do_not_learn| !do_not_learn)
        .map_err(|()| GuardianRuntimeError::LearningPreferenceUnavailable)
}

async fn learned_write_allowed(
    preferences: &dyn LearningPreferenceSource,
    owner: Option<&UserId>,
    scope: CandidateScope,
    action: MutationAction,
) -> Result<bool, GuardianRuntimeError> {
    if action != MutationAction::Commit {
        // Corrections, decay, and forget are explicit governance operations,
        // not new learning.
        return Ok(true);
    }
    match scope {
        CandidateScope::Relationship
        | CandidateScope::UserProfile
        | CandidateScope::AgentCanonical
        | CandidateScope::WorkspaceKnowledge => learning_source_allowed(preferences, owner).await,
    }
}

impl GuardianRuntimeInner {
    async fn supervise(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                break;
            }
            let now = sylvander_agent::session::now_secs();
            for _ in 0..MAX_RUNS_PER_PASS {
                match self.drain_once(now).await {
                    Ok(true) => {
                        *self.last_error.write().await = None;
                    }
                    Ok(false) => {
                        *self.last_error.write().await = None;
                        break;
                    }
                    Err(error) => {
                        *self.last_error.write().await = Some(error.to_string());
                        break;
                    }
                }
            }
            tokio::select! {
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                () = tokio::time::sleep(self.settings.poll_interval) => {}
            }
        }
    }

    async fn rotate_if_needed(&self, now: i64) -> Result<(), GuardianRuntimeError> {
        let should_rotate = {
            let epoch = self.epoch.read().await;
            epoch
                .identity
                .authorize(
                    &epoch.identity,
                    now.saturating_add(self.settings.lease_secs),
                )
                .is_err()
        };
        if !should_rotate {
            return Ok(());
        }
        let next_revision = self
            .epoch
            .read()
            .await
            .credential_revision
            .checked_add(1)
            .ok_or(GuardianRuntimeError::InvalidConfiguration)?;
        let replacement = GuardianEpoch::open(&self.settings, next_revision, now).await?;
        *self.epoch.write().await = replacement;
        Ok(())
    }

    async fn drain_once(&self, now: i64) -> Result<bool, GuardianRuntimeError> {
        self.rotate_if_needed(now).await?;
        let epoch = self.epoch.read().await;
        let Some(claim) = epoch
            .store
            .claim_next_run(
                &epoch.identity,
                self.settings.curator_version.clone(),
                now,
                self.settings.lease_secs,
            )
            .await?
        else {
            return Ok(false);
        };
        let claim = epoch
            .store
            .renew_run(&epoch.identity, &claim, now, self.settings.lease_secs)
            .await?;
        match self.process_claim(&epoch, &claim, now).await {
            Ok(()) => Ok(true),
            Err(error) => {
                let code = error.reason_code();
                if claim.attempt < self.settings.max_attempts {
                    epoch
                        .store
                        .retry_run(
                            &epoch.identity,
                            &claim,
                            code,
                            now,
                            now.saturating_add(self.settings.retry_delay_secs),
                        )
                        .await?;
                } else {
                    epoch
                        .store
                        .fail_run(&epoch.identity, &claim, code, now)
                        .await?;
                }
                Err(error)
            }
        }
    }

    async fn process_claim(
        &self,
        epoch: &GuardianEpoch,
        claim: &ClaimedCuratorRun,
        now: i64,
    ) -> Result<(), GuardianRuntimeError> {
        let event = epoch
            .store
            .event_for_claim(&epoch.identity, claim, now)
            .await?;
        match event.kind {
            GuardianEventKind::UserConfirmationReceived => {
                let confirmation = self.canonical.confirmation_for_event(&event).await?;
                // Preference lookup failure is a denial, not a stranded
                // originating run: confirmation can never weaken fail-closed
                // learning policy.
                let learning_allowed = learning_source_allowed(
                    self.learning_preferences.as_ref(),
                    event.owner.user_id.as_ref(),
                )
                .await
                .unwrap_or(false);
                epoch
                    .store
                    .confirm_from_event(
                        &epoch.identity,
                        claim,
                        &confirmation.candidate_id,
                        confirmation.expected_revision,
                        confirmation.confirmed && learning_allowed,
                        now,
                    )
                    .await?;
                epoch
                    .store
                    .finalize_run(&epoch.identity, claim, now)
                    .await?;
                return Ok(());
            }
            GuardianEventKind::RetentionSweep => {
                self.canonical.expire_owner(&event.owner, now).await?;
                epoch
                    .store
                    .finalize_run(&epoch.identity, claim, now)
                    .await?;
                return Ok(());
            }
            GuardianEventKind::SessionClosed | GuardianEventKind::UserFeedbackReceived => {
                epoch
                    .store
                    .finalize_run(&epoch.identity, claim, now)
                    .await?;
                return Ok(());
            }
            GuardianEventKind::MemoryCandidateCreated => {}
        }
        if !learning_source_allowed(
            self.learning_preferences.as_ref(),
            event.owner.user_id.as_ref(),
        )
        .await?
        {
            epoch
                .store
                .reject_run_for_learning_opt_out(&epoch.identity, claim, now)
                .await?;
            return Ok(());
        }
        let payload = self.canonical.payload_for_event(&event).await?;
        let mut candidate = epoch
            .store
            .extract_candidate(
                &epoch.identity,
                claim,
                CandidateDraft {
                    source_key: payload.source_key.clone(),
                    content: payload.content,
                    evidence: payload.evidence,
                    origin: payload.origin,
                },
                now,
            )
            .await?;
        let guardian_owner = RuntimeOwnerScope::guardian(
            candidate.owner_agent_id.clone(),
            candidate.owner_user_id.clone(),
            candidate.workspace_id.iter().cloned().collect(),
        );
        let snapshot =
            epoch
                .capabilities
                .begin_guardian_run(&epoch.identity, guardian_owner, now)?;
        if snapshot.actor() != CapabilityActor::Guardian
            || snapshot.definitions().len() != 1
            || snapshot.revision().is_empty()
        {
            return Err(GuardianRuntimeError::InvalidConfiguration);
        }
        snapshot
            .invoke("guardian.runtime_metadata", &json!({}), now)
            .await?;
        if candidate.state == CandidateState::Extracted {
            candidate = epoch
                .store
                .classify_candidate(
                    &epoch.identity,
                    claim,
                    &candidate.candidate_id,
                    candidate.revision,
                    CandidateClassification {
                        scope: payload.classification.scope,
                        confidence_basis_points: payload.classification.confidence_basis_points,
                        sensitivity: payload.classification.sensitivity,
                        retention_secs: payload.classification.retention_secs,
                        dedupe_key: payload.classification.dedupe_key,
                        workspace_id: payload.classification.workspace_id,
                    },
                    now,
                )
                .await?;
        }
        if candidate.state == CandidateState::Classified {
            candidate = epoch
                .store
                .reconcile_candidate(
                    &epoch.identity,
                    claim,
                    &candidate.candidate_id,
                    candidate.revision,
                    Reconciliation::Unique,
                    now,
                )
                .await?;
        }
        if candidate.state == CandidateState::AwaitingConfirmation {
            epoch
                .store
                .wait_for_confirmation(&epoch.identity, claim, &candidate.candidate_id, now)
                .await?;
            return Ok(());
        }
        if candidate.state == CandidateState::PolicyPending {
            let decision = epoch
                .store
                .evaluate_policy(
                    &epoch.identity,
                    claim,
                    &candidate.candidate_id,
                    candidate.revision,
                    now,
                )
                .await?;
            if decision.outcome != PolicyOutcome::Allow {
                epoch
                    .store
                    .finalize_run(&epoch.identity, claim, now)
                    .await?;
                return Ok(());
            }
            candidate = epoch.store.candidate(&candidate.candidate_id).await?;
        }
        if candidate.state == CandidateState::Authorized {
            epoch
                .store
                .schedule_mutation(
                    &epoch.identity,
                    claim,
                    &candidate.candidate_id,
                    candidate.revision,
                    now,
                )
                .await?;
        }
        self.deliver_mutations(epoch, now).await?;
        let candidate = epoch.store.candidate(&candidate.candidate_id).await?;
        if !candidate.state.is_terminal() {
            return Err(GuardianRuntimeError::Curation(
                GuardianCurationError::Conflict,
            ));
        }
        epoch
            .store
            .finalize_run(&epoch.identity, claim, now)
            .await?;
        if !matches!(
            epoch.store.run_state(&claim.run_id).await?,
            CuratorRunState::Succeeded | CuratorRunState::Failed
        ) {
            return Err(GuardianRuntimeError::Curation(
                GuardianCurationError::Conflict,
            ));
        }
        Ok(())
    }

    async fn deliver_mutations(
        &self,
        epoch: &GuardianEpoch,
        now: i64,
    ) -> Result<(), GuardianRuntimeError> {
        for _ in 0..MAX_MUTATIONS_PER_PASS {
            let Some(mutation) = epoch
                .store
                .claim_next_mutation(&epoch.identity, now, self.settings.lease_secs)
                .await?
            else {
                break;
            };
            if !learned_write_allowed(
                self.learning_preferences.as_ref(),
                mutation.owner_user_id.as_ref(),
                mutation.scope,
                mutation.action,
            )
            .await?
            {
                epoch
                    .store
                    .fail_mutation(&epoch.identity, &mutation, "learning_opt_out", now, None)
                    .await?;
                continue;
            }
            match self.canonical.apply(&mutation, now).await {
                Ok(()) => {
                    epoch
                        .store
                        .acknowledge_mutation(&epoch.identity, &mutation, now)
                        .await?;
                    if epoch.store.mutation_state(&mutation.mutation_id).await?
                        != MutationDeliveryState::Completed
                    {
                        return Err(GuardianRuntimeError::Curation(
                            GuardianCurationError::Conflict,
                        ));
                    }
                }
                Err(GuardianMutationError::Retryable) => {
                    epoch
                        .store
                        .fail_mutation(
                            &epoch.identity,
                            &mutation,
                            "canonical_sink_retryable",
                            now,
                            Some(now.saturating_add(self.settings.retry_delay_secs)),
                        )
                        .await?;
                    if epoch.store.mutation_state(&mutation.mutation_id).await?
                        != MutationDeliveryState::Pending
                    {
                        return Err(GuardianRuntimeError::Curation(
                            GuardianCurationError::Conflict,
                        ));
                    }
                    return Err(GuardianRuntimeError::MutationRetryable);
                }
                Err(GuardianMutationError::Permanent) => {
                    epoch
                        .store
                        .fail_mutation(
                            &epoch.identity,
                            &mutation,
                            "canonical_sink_rejected",
                            now,
                            None,
                        )
                        .await?;
                    if epoch.store.mutation_state(&mutation.mutation_id).await?
                        != MutationDeliveryState::DeadLetter
                    {
                        return Err(GuardianRuntimeError::Curation(
                            GuardianCurationError::Conflict,
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
struct GuardianCanonicalStore {
    connection: Arc<Mutex<Connection>>,
}

impl GuardianCanonicalStore {
    async fn open(path: impl AsRef<Path>) -> Result<Self, GuardianRuntimeError> {
        let path = path.as_ref().to_path_buf();
        let connection = tokio::task::spawn_blocking(move || {
            let mut connection =
                Connection::open(path).map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            initialize_canonical_schema(&mut connection)?;
            Ok::<_, GuardianRuntimeError>(connection)
        })
        .await
        .map_err(|_| GuardianRuntimeError::Supervisor)??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    async fn stage_payload(
        &self,
        intake_id: &str,
        owner: &RuntimeOwnerScope,
        payload: &StagedCandidatePayload,
        payload_digest: &str,
        now: i64,
    ) -> Result<(), GuardianRuntimeError> {
        let intake_id = intake_id.to_owned();
        let owner_user_id = owner.user_id.as_ref().map(|id| id.0.clone());
        let owner_agent_id = owner.agent_id.0.clone();
        let workspace_ids_json =
            serde_json::to_string(&owner.workspace_ids.iter().cloned().collect::<Vec<_>>())
                .map_err(|_| GuardianRuntimeError::InvalidConfiguration)?;
        let payload_json = serde_json::to_string(payload)
            .map_err(|_| GuardianRuntimeError::InvalidConfiguration)?;
        let payload_digest = payload_digest.to_owned();
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            let changed = connection
                .execute(
                    "INSERT OR IGNORE INTO guardian_learning_payloads(intake_id,owner_user_id,owner_agent_id,workspace_ids_json,payload_json,payload_digest,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![intake_id, owner_user_id, owner_agent_id, workspace_ids_json, payload_json, payload_digest, now],
                )
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            if changed == 0 {
                let matches: i64 = connection
                    .query_row(
                        "SELECT COUNT(*) FROM guardian_learning_payloads WHERE intake_id=?1 AND owner_user_id IS ?2 AND owner_agent_id=?3 AND workspace_ids_json=?4 AND payload_json=?5 AND payload_digest=?6",
                        params![intake_id, owner_user_id, owner_agent_id, workspace_ids_json, payload_json, payload_digest],
                        |row| row.get(0),
                    )
                    .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
                if matches != 1 {
                    return Err(GuardianRuntimeError::InvalidConfiguration);
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| GuardianRuntimeError::Supervisor)?
    }

    async fn payload_for_event(
        &self,
        event: &GuardianEvent,
    ) -> Result<StagedCandidatePayload, GuardianRuntimeError> {
        let reference = event
            .evidence
            .iter()
            .find(|evidence| evidence.kind == "worker_memory_candidate")
            .ok_or(GuardianRuntimeError::InvalidConfiguration)?;
        if reference.digest != event.payload_digest {
            return Err(GuardianRuntimeError::InvalidConfiguration);
        }
        let intake_id = reference.reference.clone();
        let owner_user_id = event.owner.user_id.as_ref().map(|id| id.0.clone());
        let owner_agent_id = event.owner.agent_id.0.clone();
        let workspace_ids_json = serde_json::to_string(
            &event
                .owner
                .workspace_ids
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
        )
        .map_err(|_| GuardianRuntimeError::InvalidConfiguration)?;
        let payload_digest = event.payload_digest.clone();
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            let payload_json = connection
                .query_row(
                    "SELECT payload_json FROM guardian_learning_payloads WHERE intake_id=?1 AND owner_user_id IS ?2 AND owner_agent_id=?3 AND workspace_ids_json=?4 AND payload_digest=?5",
                    params![intake_id, owner_user_id, owner_agent_id, workspace_ids_json, payload_digest],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?
                .ok_or(GuardianRuntimeError::InvalidConfiguration)?;
            serde_json::from_str(&payload_json)
                .map_err(|_| GuardianRuntimeError::InvalidConfiguration)
        })
        .await
        .map_err(|_| GuardianRuntimeError::Supervisor)?
    }

    async fn payload_for_candidate(
        &self,
        candidate: &crate::guardian_curation::MemoryCandidate,
    ) -> Result<StagedCandidatePayload, GuardianRuntimeError> {
        let reference = candidate
            .evidence
            .iter()
            .find(|evidence| evidence.kind == "worker_memory_candidate")
            .ok_or(GuardianRuntimeError::InvalidConfiguration)?
            .clone();
        let owner = RuntimeOwnerScope::guardian(
            candidate.owner_agent_id.clone(),
            candidate.owner_user_id.clone(),
            candidate.workspace_id.iter().cloned().collect(),
        );
        self.payload_for_event(&GuardianEvent::new(
            "candidate-payload-read",
            GuardianEventKind::MemoryCandidateCreated,
            owner,
            vec![reference.clone()],
            reference.digest,
            0,
        ))
        .await
    }

    #[allow(dead_code)] // reached through the product confirmation entry
    async fn stage_confirmation(
        &self,
        confirmation_id: &str,
        owner: &RuntimeOwnerScope,
        confirmation: &StagedConfirmation,
        digest: &str,
        now: i64,
    ) -> Result<i64, GuardianRuntimeError> {
        let confirmation_id = confirmation_id.to_owned();
        let owner_user_id = owner.user_id.as_ref().map(|id| id.0.clone());
        let owner_agent_id = owner.agent_id.0.clone();
        let workspace_ids_json =
            serde_json::to_string(&owner.workspace_ids.iter().cloned().collect::<Vec<_>>())
                .map_err(|_| GuardianRuntimeError::InvalidConfiguration)?;
        let confirmation_json = serde_json::to_string(confirmation)
            .map_err(|_| GuardianRuntimeError::InvalidConfiguration)?;
        let digest = digest.to_owned();
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            let changed = connection
                .execute(
                    "INSERT OR IGNORE INTO guardian_confirmation_payloads(confirmation_id,owner_user_id,owner_agent_id,workspace_ids_json,confirmation_json,payload_digest,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![confirmation_id, owner_user_id, owner_agent_id, workspace_ids_json, confirmation_json, digest, now],
                )
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            if changed == 0 {
                let created_at = connection
                    .query_row(
                        "SELECT created_at FROM guardian_confirmation_payloads WHERE confirmation_id=?1 AND owner_user_id IS ?2 AND owner_agent_id=?3 AND workspace_ids_json=?4 AND confirmation_json=?5 AND payload_digest=?6",
                        params![confirmation_id, owner_user_id, owner_agent_id, workspace_ids_json, confirmation_json, digest],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|_| GuardianRuntimeError::CanonicalStorage)?
                    .ok_or(GuardianRuntimeError::InvalidConfiguration)?;
                return Ok(created_at);
            }
            Ok(now)
        })
        .await
        .map_err(|_| GuardianRuntimeError::Supervisor)?
    }

    async fn confirmation_for_event(
        &self,
        event: &GuardianEvent,
    ) -> Result<StagedConfirmation, GuardianRuntimeError> {
        let reference = event
            .evidence
            .iter()
            .find(|evidence| evidence.kind == "guardian_confirmation")
            .ok_or(GuardianRuntimeError::InvalidConfiguration)?;
        if reference.digest != event.payload_digest {
            return Err(GuardianRuntimeError::InvalidConfiguration);
        }
        let confirmation_id = reference.reference.clone();
        let owner_user_id = event.owner.user_id.as_ref().map(|id| id.0.clone());
        let owner_agent_id = event.owner.agent_id.0.clone();
        let workspace_ids_json = serde_json::to_string(
            &event
                .owner
                .workspace_ids
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
        )
        .map_err(|_| GuardianRuntimeError::InvalidConfiguration)?;
        let payload_digest = event.payload_digest.clone();
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            let confirmation_json = connection
                .query_row(
                    "SELECT confirmation_json FROM guardian_confirmation_payloads WHERE confirmation_id=?1 AND owner_user_id IS ?2 AND owner_agent_id=?3 AND workspace_ids_json=?4 AND payload_digest=?5",
                    params![confirmation_id, owner_user_id, owner_agent_id, workspace_ids_json, payload_digest],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?
                .ok_or(GuardianRuntimeError::InvalidConfiguration)?;
            serde_json::from_str(&confirmation_json)
                .map_err(|_| GuardianRuntimeError::InvalidConfiguration)
        })
        .await
        .map_err(|_| GuardianRuntimeError::Supervisor)?
    }

    async fn expire_owner(
        &self,
        owner: &RuntimeOwnerScope,
        now: i64,
    ) -> Result<usize, GuardianRuntimeError> {
        let owner_user_id = owner.user_id.as_ref().map(|id| id.0.clone());
        let owner_agent_id = owner.agent_id.0.clone();
        let workspace_ids = owner.workspace_ids.clone();
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            let mut statement = connection
                .prepare(
                    "SELECT candidate_id,scope,workspace_id FROM guardian_canonical_memory WHERE deleted=0 AND expires_at IS NOT NULL AND expires_at<=?1 AND owner_agent_id=?2 AND owner_user_id IS ?3",
                )
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            let candidates = statement
                .query_map(params![now, owner_agent_id, owner_user_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            drop(statement);
            let mut changed = 0;
            for (candidate_id, scope, workspace_id) in candidates {
                if scope == "workspace_knowledge"
                    && workspace_id
                        .as_ref()
                        .is_none_or(|workspace| !workspace_ids.contains(workspace))
                {
                    continue;
                }
                changed += connection
                    .execute(
                        "UPDATE guardian_canonical_memory SET deleted=1,body_json=NULL,updated_at=?2 WHERE candidate_id=?1 AND deleted=0",
                        params![candidate_id, now],
                    )
                    .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
            }
            Ok(changed)
        })
        .await
        .map_err(|_| GuardianRuntimeError::Supervisor)?
    }

    async fn apply(
        &self,
        mutation: &ClaimedMutation,
        now: i64,
    ) -> Result<(), GuardianMutationError> {
        if !valid_mutation_owner(mutation) {
            return Err(GuardianMutationError::Permanent);
        }
        let mutation = mutation.clone();
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| GuardianMutationError::Retryable)?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| GuardianMutationError::Retryable)?;
            let body_json = serde_json::to_string(&mutation.body)
                .map_err(|_| GuardianMutationError::Permanent)?;
            let body_digest =
                domain_digest(b"sylvander.guardian.mutation-body.v1\0", &body_json);
            let previous = transaction
                .query_row(
                    "SELECT mutation_id,body_digest FROM guardian_mutation_receipts WHERE idempotency_key=?1",
                    [&mutation.idempotency_key],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|_| GuardianMutationError::Retryable)?;
            if let Some((mutation_id, digest)) = previous {
                return if mutation_id == mutation.mutation_id && digest == body_digest {
                    Ok(())
                } else {
                    Err(GuardianMutationError::Permanent)
                };
            }
            apply_canonical_mutation(&transaction, &mutation, &body_json, now)?;
            transaction
                .execute(
                    "INSERT INTO guardian_mutation_receipts(idempotency_key,mutation_id,body_digest,applied_at) VALUES (?1,?2,?3,?4)",
                    params![
                        mutation.idempotency_key,
                        mutation.mutation_id,
                        body_digest,
                        now
                    ],
                )
                .map_err(|_| GuardianMutationError::Retryable)?;
            transaction
                .commit()
                .map_err(|_| GuardianMutationError::Retryable)
        })
        .await
        .map_err(|_| GuardianMutationError::Retryable)?
    }
}

#[async_trait]
impl CuratedContextProvider for GuardianCanonicalStore {
    async fn retrieve(
        &self,
        subject: &CuratedContextSubject,
        query: &str,
        max_items: usize,
    ) -> Result<Vec<CuratedContextEntry>, MemoryCandidateError> {
        if subject.user_id.0.is_empty()
            || subject.agent_id.0.is_empty()
            || subject.session_id.0.is_empty()
            || query.len() > 4 * 1024
            || max_items == 0
            || max_items > 64
        {
            return Err(MemoryCandidateError::Invalid);
        }
        let user_id = subject.user_id.0.clone();
        let agent_id = subject.agent_id.0.clone();
        let workspace_ids = subject
            .workspace_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let terms = query_terms(query);
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| MemoryCandidateError::Unavailable)?;
            let mut statement = connection
                .prepare(
                    "SELECT candidate_id,scope,owner_user_id,owner_agent_id,workspace_id,revision,body_json,expires_at
                     FROM guardian_canonical_memory
                     WHERE deleted=0 AND (expires_at IS NULL OR expires_at>?1)
                       AND (owner_agent_id=?2 OR owner_user_id=?3)
                     ORDER BY updated_at DESC,candidate_id",
                )
                .map_err(|_| MemoryCandidateError::Unavailable)?;
            let now = now_seconds();
            let rows = statement
                .query_map(params![now, agent_id, user_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                    ))
                })
                .map_err(|_| MemoryCandidateError::Unavailable)?;
            let mut entries = Vec::new();
            for row in rows {
                let (
                    candidate_id,
                    scope,
                    owner_user,
                    owner_agent,
                    workspace_id,
                    revision,
                    body_json,
                    expires_at,
                ) = row.map_err(|_| MemoryCandidateError::Unavailable)?;
                let scope = parse_curated_scope(&scope)?;
                let visible = match scope {
                    CuratedMemoryScope::Relationship => {
                        owner_user.as_deref() == Some(user_id.as_str()) && owner_agent == agent_id
                    }
                    CuratedMemoryScope::UserProfile => {
                        owner_user.as_deref() == Some(user_id.as_str())
                    }
                    CuratedMemoryScope::AgentCanonical => owner_agent == agent_id,
                    CuratedMemoryScope::WorkspaceKnowledge => workspace_id
                        .as_ref()
                        .is_some_and(|id| workspace_ids.contains(id)),
                };
                if !visible {
                    continue;
                }
                let body: Value = serde_json::from_str(&body_json)
                    .map_err(|_| MemoryCandidateError::Unavailable)?;
                let Some(content) = body
                    .get("content")
                    .and_then(|value| value.get("text"))
                    .and_then(Value::as_str)
                else {
                    return Err(MemoryCandidateError::Unavailable);
                };
                let relevance = relevance_score(content, &terms);
                if !terms.is_empty() && relevance == 0 {
                    continue;
                }
                entries.push(CuratedContextEntry {
                    scope,
                    content: content.to_owned(),
                    reference: format!("guardian:{candidate_id}"),
                    revision: u64::try_from(revision)
                        .map_err(|_| MemoryCandidateError::Unavailable)?,
                    expires_at_unix_secs: expires_at,
                    relevance,
                });
                if entries.len() == max_items {
                    break;
                }
            }
            Ok(entries)
        })
        .await
        .map_err(|_| MemoryCandidateError::Unavailable)?
    }
}

fn apply_canonical_mutation(
    transaction: &rusqlite::Transaction<'_>,
    mutation: &ClaimedMutation,
    body_json: &str,
    now: i64,
) -> Result<(), GuardianMutationError> {
    let revision =
        i64::try_from(mutation.candidate_revision).map_err(|_| GuardianMutationError::Permanent)?;
    let changed = match mutation.action {
        MutationAction::Commit => transaction.execute(
            "INSERT INTO guardian_canonical_memory(candidate_id,scope,owner_user_id,owner_agent_id,workspace_id,revision,body_json,expires_at,deleted,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,0,?9)",
            params![mutation.candidate_id, canonical_scope_value(mutation.scope), mutation.owner_user_id.as_ref().map(|id| id.0.as_str()), mutation.owner_agent_id.0, mutation.workspace_id, revision, body_json, mutation.body.get("expires_at_unix_secs").and_then(Value::as_i64), now],
        ),
        MutationAction::Correct => transaction.execute(
            "UPDATE guardian_canonical_memory SET revision=?3,body_json=?4,expires_at=?5,deleted=0,updated_at=?6 WHERE candidate_id=?1 AND owner_agent_id=?2 AND revision<?3",
            params![mutation.candidate_id, mutation.owner_agent_id.0, revision, body_json, mutation.body.get("expires_at_unix_secs").and_then(Value::as_i64), now],
        ),
        MutationAction::Decay | MutationAction::Forget => transaction.execute(
            "UPDATE guardian_canonical_memory SET revision=?3,body_json=NULL,deleted=1,updated_at=?4 WHERE candidate_id=?1 AND owner_agent_id=?2 AND revision<?3",
            params![mutation.candidate_id, mutation.owner_agent_id.0, revision, now],
        ),
    };
    match changed {
        Ok(1) => Ok(()),
        Ok(_) | Err(rusqlite::Error::SqliteFailure(_, _)) => Err(GuardianMutationError::Permanent),
        Err(_) => Err(GuardianMutationError::Retryable),
    }
}

fn valid_mutation_owner(mutation: &ClaimedMutation) -> bool {
    !mutation.owner_agent_id.0.is_empty()
        && match mutation.scope {
            CandidateScope::Relationship | CandidateScope::UserProfile => {
                mutation
                    .owner_user_id
                    .as_ref()
                    .is_some_and(|id| !id.0.is_empty())
                    && mutation.workspace_id.is_none()
            }
            CandidateScope::AgentCanonical => mutation.workspace_id.is_none(),
            CandidateScope::WorkspaceKnowledge => mutation
                .workspace_id
                .as_ref()
                .is_some_and(|id| !id.is_empty()),
        }
}

const fn canonical_scope_value(scope: CandidateScope) -> &'static str {
    match scope {
        CandidateScope::Relationship => "relationship",
        CandidateScope::UserProfile => "user_profile",
        CandidateScope::AgentCanonical => "agent_canonical",
        CandidateScope::WorkspaceKnowledge => "workspace_knowledge",
    }
}

fn parse_curated_scope(scope: &str) -> Result<CuratedMemoryScope, MemoryCandidateError> {
    match scope {
        "relationship" => Ok(CuratedMemoryScope::Relationship),
        "user_profile" => Ok(CuratedMemoryScope::UserProfile),
        "agent_canonical" => Ok(CuratedMemoryScope::AgentCanonical),
        "workspace_knowledge" => Ok(CuratedMemoryScope::WorkspaceKnowledge),
        _ => Err(MemoryCandidateError::Unavailable),
    }
}

fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .map(str::trim)
        .filter(|term| term.chars().count() >= 2)
        .take(8)
        .map(str::to_lowercase)
        .collect()
}

fn relevance_score(content: &str, terms: &[String]) -> u16 {
    let content = content.to_lowercase();
    terms.iter().fold(0_u16, |score, term| {
        score.saturating_add(if content.contains(term) { 1_000 } else { 0 })
    })
}

fn digest_value(domain: &[u8], value: &Value) -> Result<String, serde_json::Error> {
    let encoded = serde_json::to_vec(value)?;
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(encoded);
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn now_seconds() -> i64 {
    sylvander_agent::session::now_secs()
}

fn initialize_canonical_schema(connection: &mut Connection) -> Result<(), GuardianRuntimeError> {
    let application_id: i64 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
    let user_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
    if application_id == 0 && user_version == 0 {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
        transaction
            .execute_batch(
                "CREATE TABLE guardian_canonical_memory(
                    candidate_id TEXT PRIMARY KEY,
                    scope TEXT NOT NULL CHECK(scope IN ('relationship','user_profile','agent_canonical','workspace_knowledge')),
                    owner_user_id TEXT,
                    owner_agent_id TEXT NOT NULL,
                    workspace_id TEXT,
                    revision INTEGER NOT NULL CHECK(revision > 0),
                    body_json TEXT,
                    expires_at INTEGER,
                    deleted INTEGER NOT NULL CHECK(deleted IN (0,1)),
                    updated_at INTEGER NOT NULL,
                    CHECK((scope='relationship' AND owner_user_id IS NOT NULL AND workspace_id IS NULL)
                       OR (scope='user_profile' AND owner_user_id IS NOT NULL AND workspace_id IS NULL)
                       OR (scope='agent_canonical' AND workspace_id IS NULL)
                       OR (scope='workspace_knowledge' AND workspace_id IS NOT NULL))
                ) STRICT;
                CREATE INDEX guardian_canonical_visibility
                    ON guardian_canonical_memory(scope,owner_agent_id,owner_user_id,workspace_id,deleted);
                CREATE TABLE guardian_learning_payloads(
                    intake_id TEXT PRIMARY KEY,
                    owner_user_id TEXT,
                    owner_agent_id TEXT NOT NULL,
                    workspace_ids_json TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    payload_digest TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                ) STRICT;
                CREATE TABLE guardian_confirmation_payloads(
                    confirmation_id TEXT PRIMARY KEY,
                    owner_user_id TEXT,
                    owner_agent_id TEXT NOT NULL,
                    workspace_ids_json TEXT NOT NULL,
                    confirmation_json TEXT NOT NULL,
                    payload_digest TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                ) STRICT;
                CREATE TABLE guardian_mutation_receipts(
                    idempotency_key TEXT PRIMARY KEY,
                    mutation_id TEXT NOT NULL,
                    body_digest TEXT NOT NULL,
                    applied_at INTEGER NOT NULL
                ) STRICT;",
            )
            .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
        transaction
            .pragma_update(None, "application_id", CANONICAL_APPLICATION_ID)
            .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
        transaction
            .pragma_update(None, "user_version", CANONICAL_SCHEMA_VERSION)
            .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
        transaction
            .commit()
            .map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
    } else if application_id != CANONICAL_APPLICATION_ID || user_version != CANONICAL_SCHEMA_VERSION
    {
        return Err(GuardianRuntimeError::IncompatibleCanonicalSchema);
    }
    Ok(())
}

fn create_parent(path: &Path) -> Result<(), GuardianRuntimeError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|_| GuardianRuntimeError::CanonicalStorage)?;
    }
    Ok(())
}

fn owner_from_session(session: &StoredSession) -> Result<RuntimeOwnerScope, GuardianRuntimeError> {
    let [agent_id] = session.agents.as_slice() else {
        return Err(GuardianRuntimeError::InvalidConfiguration);
    };
    if session.metadata.user_id.trim().is_empty() {
        return Err(GuardianRuntimeError::InvalidConfiguration);
    }
    Ok(RuntimeOwnerScope::guardian(
        agent_id.clone(),
        Some(UserId::new(&session.metadata.user_id)),
        BTreeSet::new(),
    ))
}

fn confirmation_summary(content: &Value) -> String {
    let source = content
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("A new memory candidate");
    let mut summary = String::with_capacity(source.len().min(480));
    for character in source.chars() {
        if summary.len().saturating_add(character.len_utf8()) > 480 {
            break;
        }
        if matches!(character, '\n' | '\r' | '\t') {
            summary.push(' ');
        } else if !character.is_control() {
            summary.push(character);
        }
    }
    let summary = summary.trim();
    if summary.is_empty() {
        "A new memory candidate".into()
    } else {
        summary.into()
    }
}

fn domain_digest(domain: &[u8], value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(value.as_bytes());
    format!("sha256:{:x}", digest.finalize())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum GuardianRuntimeError {
    #[error("Guardian runtime configuration is invalid")]
    InvalidConfiguration,
    #[error("Guardian canonical store schema is incompatible")]
    IncompatibleCanonicalSchema,
    #[error("Guardian canonical storage failed")]
    CanonicalStorage,
    #[error("Guardian mutation is retryable")]
    MutationRetryable,
    #[error("Guardian supervisor failed")]
    Supervisor,
    #[error("Guardian learning preference is unavailable")]
    LearningPreferenceUnavailable,
    #[error(transparent)]
    Capability(#[from] CapabilityRuntimeError),
    #[error(transparent)]
    Curation(#[from] GuardianCurationError),
}

impl GuardianRuntimeError {
    fn reason_code(&self) -> &'static str {
        match self {
            Self::MutationRetryable => "mutation_retryable",
            Self::LearningPreferenceUnavailable => "learning_preference_unavailable",
            Self::Capability(_) => "capability_failed",
            Self::Curation(_) => "curation_failed",
            Self::InvalidConfiguration
            | Self::IncompatibleCanonicalSchema
            | Self::CanonicalStorage
            | Self::Supervisor => "guardian_runtime_failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardianMutationError {
    Retryable,
    Permanent,
}

#[cfg(test)]
#[path = "../tests/unit/guardian_runtime.rs"]
mod tests;
