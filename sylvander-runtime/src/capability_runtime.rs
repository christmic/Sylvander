//! Actor-separated capability discovery and invocation.
//!
//! A capability is visible only through an immutable per-run snapshot. The
//! snapshot is not authority by itself: every invocation is checked again by
//! the policy gateway and durably audited before its handler can run.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sylvander_protocol::{AgentId, SessionContext, SessionId, UserId};
use thiserror::Error;
use uuid::Uuid;

/// Runtime actor classes. Guardian is deliberately not a Worker role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CapabilityActor {
    Worker,
    Guardian,
}

/// Security class used by the deterministic policy gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CapabilityClass {
    Read,
    SessionAppend,
    RelationshipAppend,
    AgentCandidateAppend,
    UserProfileMutation,
    CanonicalMemoryMutation,
    WorkspaceKnowledgeMutation,
    WorkspaceMutation,
    Terminal,
    Browser,
    HostControl,
    ArbitraryMcp,
    Extension,
}

impl CapabilityClass {
    const fn allowed_for(self, actor: CapabilityActor) -> bool {
        match actor {
            CapabilityActor::Worker => !matches!(
                self,
                Self::UserProfileMutation
                    | Self::CanonicalMemoryMutation
                    | Self::WorkspaceKnowledgeMutation
            ),
            CapabilityActor::Guardian => !matches!(
                self,
                Self::SessionAppend
                    | Self::RelationshipAppend
                    | Self::AgentCandidateAppend
                    | Self::WorkspaceMutation
                    | Self::Terminal
                    | Self::Browser
                    | Self::HostControl
                    | Self::ArbitraryMcp
                    | Self::Extension
            ),
        }
    }
}

/// Content-safe schema metadata published during capability discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CapabilityDefinition {
    pub(crate) name: String,
    pub(crate) version: u64,
    pub(crate) class: CapabilityClass,
    /// Digest of the separately stored JSON schema. The schema itself remains
    /// owned by the adapter and can only be returned from the correct actor's
    /// snapshot.
    pub(crate) schema_digest: String,
    pub(crate) schema: Value,
}

/// Runtime-owned owner scope. Capability inputs never establish these IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeOwnerScope {
    pub(crate) user_id: Option<UserId>,
    pub(crate) agent_id: AgentId,
    pub(crate) session_id: Option<SessionId>,
    pub(crate) workspace_ids: BTreeSet<String>,
}

impl RuntimeOwnerScope {
    pub(crate) fn worker(session: &SessionContext, workspace_ids: BTreeSet<String>) -> Self {
        Self {
            user_id: Some(session.identity.user_id.clone()),
            agent_id: session.identity.agent_id.clone(),
            session_id: Some(session.identity.session_id.clone()),
            workspace_ids,
        }
    }

    pub(crate) fn guardian(
        agent_id: AgentId,
        user_id: Option<UserId>,
        workspace_ids: BTreeSet<String>,
    ) -> Self {
        Self {
            user_id,
            agent_id,
            session_id: None,
            workspace_ids,
        }
    }

    fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"sylvander.capability.owner.v1\0");
        if let Some(user_id) = &self.user_id {
            hasher.update(user_id.0.as_bytes());
        }
        hasher.update([0]);
        hasher.update(self.agent_id.0.as_bytes());
        hasher.update([0]);
        if let Some(session_id) = &self.session_id {
            hasher.update(session_id.0.as_bytes());
        }
        for workspace_id in &self.workspace_ids {
            hasher.update([0]);
            hasher.update(workspace_id.as_bytes());
        }
        format!("sha256:{:x}", hasher.finalize())
    }
}

/// Expiring Guardian identity issued by Runtime, never by a model or channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuardianServiceIdentity {
    service_id: String,
    credential_revision: u64,
    expires_at_unix_secs: i64,
}

impl GuardianServiceIdentity {
    pub(crate) fn issue(
        service_id: impl Into<String>,
        credential_revision: u64,
        expires_at_unix_secs: i64,
    ) -> Result<Self, CapabilityRuntimeError> {
        let service_id = service_id.into();
        if service_id.trim().is_empty() || credential_revision == 0 || expires_at_unix_secs <= 0 {
            return Err(CapabilityRuntimeError::InvalidConfiguration);
        }
        Ok(Self {
            service_id,
            credential_revision,
            expires_at_unix_secs,
        })
    }

    pub(crate) fn authorize(
        &self,
        expected: &Self,
        now_unix_secs: i64,
    ) -> Result<(), CapabilityRuntimeError> {
        if self != expected || self.expires_at_unix_secs <= now_unix_secs {
            return Err(CapabilityRuntimeError::AccessDenied);
        }
        Ok(())
    }

    pub(crate) fn content_safe_digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"sylvander.guardian.service.v1\0");
        hasher.update(self.service_id.as_bytes());
        format!("sha256:{:x}", hasher.finalize())
    }
}

/// Invocation data visible to a capability implementation only after policy
/// authorization and successful pre-execution audit.
#[derive(Debug)]
pub(crate) struct AuthorizedCapabilityInvocation<'a> {
    pub(crate) invocation_id: &'a str,
    pub(crate) actor: CapabilityActor,
    pub(crate) owner: &'a RuntimeOwnerScope,
    pub(crate) input: &'a Value,
}

#[async_trait]
pub(crate) trait RuntimeCapability: Send + Sync {
    fn definition(&self) -> CapabilityDefinition;

    async fn invoke(
        &self,
        invocation: AuthorizedCapabilityInvocation<'_>,
    ) -> Result<Value, CapabilityRuntimeError>;
}

#[derive(Default)]
pub(crate) struct CapabilityRegistry {
    capabilities: BTreeMap<String, Arc<dyn RuntimeCapability>>,
}

impl CapabilityRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register<C: RuntimeCapability + 'static>(
        mut self,
        capability: C,
    ) -> Result<Self, CapabilityRuntimeError> {
        let definition = capability.definition();
        validate_definition(&definition)?;
        if self
            .capabilities
            .insert(definition.name, Arc::new(capability))
            .is_some()
        {
            return Err(CapabilityRuntimeError::InvalidConfiguration);
        }
        Ok(self)
    }
}

/// A content-free policy/audit record. Inputs, outputs, owner IDs, and service
/// credentials are intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityAuditRecord {
    pub(crate) invocation_id: String,
    pub(crate) phase: CapabilityAuditPhase,
    pub(crate) actor: CapabilityActor,
    pub(crate) capability: String,
    pub(crate) capability_revision: String,
    pub(crate) policy_revision: u64,
    pub(crate) owner_digest: String,
    pub(crate) outcome: CapabilityAuditOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapabilityAuditPhase {
    Authorized,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapabilityAuditOutcome {
    Allowed,
    Succeeded,
    Failed,
}

/// The sink must make `record` durable before returning. An unavailable sink
/// fails closed before capability execution.
pub(crate) trait CapabilityAuditSink: Send + Sync {
    fn record(&self, record: &CapabilityAuditRecord) -> Result<(), ()>;
}

struct ActorRegistry {
    actor: CapabilityActor,
    capabilities: BTreeMap<String, Arc<dyn RuntimeCapability>>,
    revision: String,
}

/// Runtime-owned factory for immutable Worker and Guardian run snapshots.
pub(crate) struct ActorCapabilityRuntime {
    worker: Arc<ActorRegistry>,
    guardian: Arc<ActorRegistry>,
    guardian_identity: GuardianServiceIdentity,
    policy_revision: u64,
    audit: Arc<dyn CapabilityAuditSink>,
}

impl ActorCapabilityRuntime {
    pub(crate) fn new(
        worker: CapabilityRegistry,
        guardian: CapabilityRegistry,
        guardian_identity: GuardianServiceIdentity,
        policy_revision: u64,
        audit: Arc<dyn CapabilityAuditSink>,
    ) -> Result<Self, CapabilityRuntimeError> {
        if policy_revision == 0 {
            return Err(CapabilityRuntimeError::InvalidConfiguration);
        }
        let worker = Arc::new(freeze_registry(CapabilityActor::Worker, worker)?);
        let guardian = Arc::new(freeze_registry(CapabilityActor::Guardian, guardian)?);
        if worker
            .capabilities
            .keys()
            .any(|name| guardian.capabilities.contains_key(name))
        {
            return Err(CapabilityRuntimeError::InvalidConfiguration);
        }
        Ok(Self {
            worker,
            guardian,
            guardian_identity,
            policy_revision,
            audit,
        })
    }

    pub(crate) fn begin_worker_run(
        &self,
        session: &SessionContext,
        workspace_ids: BTreeSet<String>,
    ) -> ActorCapabilitySnapshot {
        ActorCapabilitySnapshot::new(
            self.worker.clone(),
            RuntimeOwnerScope::worker(session, workspace_ids),
            None,
            self.policy_revision,
            self.audit.clone(),
        )
    }

    pub(crate) fn begin_guardian_run(
        &self,
        identity: &GuardianServiceIdentity,
        owner: RuntimeOwnerScope,
        now_unix_secs: i64,
    ) -> Result<ActorCapabilitySnapshot, CapabilityRuntimeError> {
        if identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .is_err()
            || owner.session_id.is_some()
        {
            return Err(CapabilityRuntimeError::AccessDenied);
        }
        Ok(ActorCapabilitySnapshot::new(
            self.guardian.clone(),
            owner,
            Some(identity.clone()),
            self.policy_revision,
            self.audit.clone(),
        ))
    }
}

/// Immutable discovery and invocation surface for exactly one actor run.
pub(crate) struct ActorCapabilitySnapshot {
    registry: Arc<ActorRegistry>,
    owner: RuntimeOwnerScope,
    guardian_identity: Option<GuardianServiceIdentity>,
    policy_revision: u64,
    audit: Arc<dyn CapabilityAuditSink>,
}

impl ActorCapabilitySnapshot {
    fn new(
        registry: Arc<ActorRegistry>,
        owner: RuntimeOwnerScope,
        guardian_identity: Option<GuardianServiceIdentity>,
        policy_revision: u64,
        audit: Arc<dyn CapabilityAuditSink>,
    ) -> Self {
        Self {
            registry,
            owner,
            guardian_identity,
            policy_revision,
            audit,
        }
    }

    pub(crate) fn actor(&self) -> CapabilityActor {
        self.registry.actor
    }

    pub(crate) fn revision(&self) -> &str {
        &self.registry.revision
    }

    pub(crate) fn definitions(&self) -> Vec<CapabilityDefinition> {
        self.registry
            .capabilities
            .values()
            .map(|capability| capability.definition())
            .collect()
    }

    pub(crate) async fn invoke(
        &self,
        name: &str,
        input: &Value,
        now_unix_secs: i64,
    ) -> Result<Value, CapabilityRuntimeError> {
        let capability = self
            .registry
            .capabilities
            .get(name)
            .ok_or(CapabilityRuntimeError::CapabilityUnavailable)?;
        let definition = capability.definition();
        authorize(
            self.registry.actor,
            &definition,
            &self.owner,
            self.guardian_identity.as_ref(),
            input,
            now_unix_secs,
        )?;

        let invocation_id = Uuid::new_v4().to_string();
        let base = CapabilityAuditRecord {
            invocation_id: invocation_id.clone(),
            phase: CapabilityAuditPhase::Authorized,
            actor: self.registry.actor,
            capability: definition.name,
            capability_revision: self.registry.revision.clone(),
            policy_revision: self.policy_revision,
            owner_digest: self.owner.digest(),
            outcome: CapabilityAuditOutcome::Allowed,
        };
        self.audit
            .record(&base)
            .map_err(|()| CapabilityRuntimeError::AuditUnavailable)?;

        let result = capability
            .invoke(AuthorizedCapabilityInvocation {
                invocation_id: &invocation_id,
                actor: self.registry.actor,
                owner: &self.owner,
                input,
            })
            .await;
        let terminal = CapabilityAuditRecord {
            phase: CapabilityAuditPhase::Completed,
            outcome: if result.is_ok() {
                CapabilityAuditOutcome::Succeeded
            } else {
                CapabilityAuditOutcome::Failed
            },
            ..base
        };
        self.audit
            .record(&terminal)
            .map_err(|()| CapabilityRuntimeError::ExecutionOutcomeUncertain)?;
        result
    }

    /// Authorize an executable adapter without invoking it inside Runtime.
    ///
    /// The returned lease owns the terminal audit obligation. Dropping it
    /// records a failed terminal outcome, which covers task cancellation.
    pub(crate) fn authorize_external(
        &self,
        name: &str,
        input: &Value,
        invocation_revision: &str,
        now_unix_secs: i64,
    ) -> Result<ExternalCapabilityInvocation, CapabilityRuntimeError> {
        let capability = self
            .registry
            .capabilities
            .get(name)
            .ok_or(CapabilityRuntimeError::CapabilityUnavailable)?;
        let definition = capability.definition();
        authorize(
            self.registry.actor,
            &definition,
            &self.owner,
            self.guardian_identity.as_ref(),
            input,
            now_unix_secs,
        )?;
        if !invocation_revision.starts_with("sha256:") {
            return Err(CapabilityRuntimeError::AccessDenied);
        }

        let base = CapabilityAuditRecord {
            invocation_id: Uuid::new_v4().to_string(),
            phase: CapabilityAuditPhase::Authorized,
            actor: self.registry.actor,
            capability: definition.name,
            capability_revision: invocation_revision.to_owned(),
            policy_revision: self.policy_revision,
            owner_digest: self.owner.digest(),
            outcome: CapabilityAuditOutcome::Allowed,
        };
        self.audit
            .record(&base)
            .map_err(|()| CapabilityRuntimeError::AuditUnavailable)?;
        Ok(ExternalCapabilityInvocation {
            audit: self.audit.clone(),
            terminal: Some(CapabilityAuditRecord {
                phase: CapabilityAuditPhase::Completed,
                outcome: CapabilityAuditOutcome::Failed,
                ..base
            }),
        })
    }
}

/// Terminal-audit lease for an adapter executed outside Runtime.
pub(crate) struct ExternalCapabilityInvocation {
    audit: Arc<dyn CapabilityAuditSink>,
    terminal: Option<CapabilityAuditRecord>,
}

impl ExternalCapabilityInvocation {
    pub(crate) fn finish(mut self, succeeded: bool) -> Result<(), CapabilityRuntimeError> {
        let mut terminal = self
            .terminal
            .take()
            .ok_or(CapabilityRuntimeError::ExecutionOutcomeUncertain)?;
        terminal.outcome = if succeeded {
            CapabilityAuditOutcome::Succeeded
        } else {
            CapabilityAuditOutcome::Failed
        };
        self.audit
            .record(&terminal)
            .map_err(|()| CapabilityRuntimeError::ExecutionOutcomeUncertain)
    }
}

impl Drop for ExternalCapabilityInvocation {
    fn drop(&mut self) {
        if let Some(terminal) = self.terminal.take() {
            let _ = self.audit.record(&terminal);
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum CapabilityRuntimeError {
    #[error("capability is unavailable")]
    CapabilityUnavailable,
    #[error("capability access denied")]
    AccessDenied,
    #[error("capability audit is unavailable")]
    AuditUnavailable,
    #[error("capability executed but its terminal audit outcome is uncertain")]
    ExecutionOutcomeUncertain,
    #[error("capability configuration is invalid")]
    InvalidConfiguration,
    #[error("capability execution failed")]
    ExecutionFailed,
}

fn freeze_registry(
    actor: CapabilityActor,
    registry: CapabilityRegistry,
) -> Result<ActorRegistry, CapabilityRuntimeError> {
    for capability in registry.capabilities.values() {
        if !capability.definition().class.allowed_for(actor) {
            return Err(CapabilityRuntimeError::InvalidConfiguration);
        }
    }
    let revision = registry_revision(actor, &registry.capabilities);
    Ok(ActorRegistry {
        actor,
        capabilities: registry.capabilities,
        revision,
    })
}

fn validate_definition(definition: &CapabilityDefinition) -> Result<(), CapabilityRuntimeError> {
    if definition.name.trim().is_empty()
        || definition.version == 0
        || !definition.schema.is_object()
        || definition.schema_digest != value_digest(&definition.schema)
    {
        return Err(CapabilityRuntimeError::InvalidConfiguration);
    }
    Ok(())
}

fn authorize(
    actor: CapabilityActor,
    definition: &CapabilityDefinition,
    owner: &RuntimeOwnerScope,
    guardian_identity: Option<&GuardianServiceIdentity>,
    input: &Value,
    now_unix_secs: i64,
) -> Result<(), CapabilityRuntimeError> {
    if !definition.class.allowed_for(actor)
        || owner.agent_id.0.is_empty()
        || contains_owner_selector(input)
    {
        return Err(CapabilityRuntimeError::AccessDenied);
    }
    match actor {
        CapabilityActor::Worker
            if owner.user_id.is_none()
                || owner.session_id.is_none()
                || guardian_identity.is_some() =>
        {
            Err(CapabilityRuntimeError::AccessDenied)
        }
        CapabilityActor::Guardian
            if owner.session_id.is_some()
                || guardian_identity
                    .is_none_or(|identity| identity.expires_at_unix_secs <= now_unix_secs) =>
        {
            Err(CapabilityRuntimeError::AccessDenied)
        }
        _ => Ok(()),
    }
}

fn contains_owner_selector(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "owner" | "owner_id" | "user_id" | "agent_id" | "session_id" | "workspace_id"
            ) || contains_owner_selector(value)
        }),
        Value::Array(values) => values.iter().any(contains_owner_selector),
        _ => false,
    }
}

fn registry_revision(
    actor: CapabilityActor,
    capabilities: &BTreeMap<String, Arc<dyn RuntimeCapability>>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sylvander.capability.snapshot.v1\0");
    hasher.update(match actor {
        CapabilityActor::Worker => b"worker".as_slice(),
        CapabilityActor::Guardian => b"guardian".as_slice(),
    });
    for capability in capabilities.values() {
        let definition = capability.definition();
        hasher.update([0]);
        hasher.update(definition.name.as_bytes());
        hasher.update(definition.version.to_be_bytes());
        hasher.update([definition.class as u8]);
        hasher.update(definition.schema_digest.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

pub(crate) fn value_digest(value: &Value) -> String {
    let encoded = serde_json::to_vec(value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(b"sylvander.capability.schema.v1\0");
    hasher.update(encoded);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
#[path = "../tests/unit/capability_runtime.rs"]
mod tests;
