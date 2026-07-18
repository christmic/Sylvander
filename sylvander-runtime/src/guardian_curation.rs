//! Durable Guardian curation pipeline.
//!
//! Conversation/session storage is deliberately not reused here. Immutable
//! source references enter an outbox, a leased `CuratorRun` produces typed
//! candidates, deterministic policy authorizes mutations, and an idempotent
//! mutation outbox hands approved changes to the owning memory/profile store.

#[path = "guardian_curation/models.rs"]
mod models;
#[path = "guardian_curation/policy.rs"]
mod policy;
#[path = "guardian_curation/schema.rs"]
mod schema;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sylvander_protocol::{AgentId, UserId};
use thiserror::Error;
use tokio::task;
use uuid::Uuid;

use crate::capability_runtime::{
    CapabilityActor, CapabilityAuditOutcome, CapabilityAuditPhase, CapabilityAuditRecord,
    CapabilityAuditSink, GuardianServiceIdentity, RuntimeOwnerScope,
};

pub(crate) use models::{
    CandidateClassification, CandidateCorrection, CandidateDraft, CandidateOrigin, CandidateScope,
    CandidateState, ClaimedCuratorRun, ClaimedMutation, ConflictResolution, ConsentState,
    CuratorRunState, EvidenceReference, GuardianEvent, GuardianEventKind, MemoryCandidate,
    MutationAction, MutationDeliveryState, PolicyDecision, PolicyOutcome, Reconciliation,
    Sensitivity,
};
use models::{
    MAX_CONTENT_BYTES, MAX_EVIDENCE_REFERENCES, MAX_REFERENCE_BYTES, MAX_RETENTION_SECS,
    MAX_WORKSPACE_IDS,
};
use policy::DeterministicGuardianPolicy;

const MAX_ID_BYTES: usize = 512;
const MAX_VERSION_BYTES: usize = 256;
const MAX_REASON_BYTES: usize = 256;
const MAX_LEASE_SECS: i64 = 15 * 60;
const MAX_RETRY_DELAY_SECS: i64 = 24 * 60 * 60;

/// Durable latest-schema curation store and deterministic policy boundary.
#[derive(Clone)]
pub(crate) struct GuardianCurationStore {
    connection: Arc<Mutex<Connection>>,
    guardian_identity: GuardianServiceIdentity,
    policy: Arc<DeterministicGuardianPolicy>,
}

#[allow(dead_code)] // operator-driven correction/decay/forget entry point
struct FollowupRequest {
    candidate_id: String,
    expected_revision: u64,
    action: MutationAction,
    replacement: Option<(Value, Vec<EvidenceReference>)>,
}

impl GuardianCurationStore {
    /// Open an empty/latest database and bind this service instance to the
    /// currently issued Guardian credential and deterministic policy revision.
    pub(crate) async fn open(
        path: impl AsRef<Path>,
        guardian_identity: GuardianServiceIdentity,
        policy_revision: u64,
    ) -> Result<Self, GuardianCurationError> {
        let path = path.as_ref().to_path_buf();
        let policy = DeterministicGuardianPolicy::new(policy_revision)
            .ok_or(GuardianCurationError::InvalidInput)?;
        let connection = task::spawn_blocking(move || {
            let mut connection = Connection::open(path).map_err(storage_error)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(storage_error)?;
            schema::initialize(&mut connection)?;
            Ok::<_, GuardianCurationError>(connection)
        })
        .await
        .map_err(|_| GuardianCurationError::Task)??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            guardian_identity,
            policy: Arc::new(policy),
        })
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, GuardianCurationError> + Send + 'static,
    ) -> Result<T, GuardianCurationError> {
        let connection = self.connection.clone();
        task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| GuardianCurationError::Storage)?;
            operation(&mut connection)
        })
        .await
        .map_err(|_| GuardianCurationError::Task)?
    }

    /// Idempotently enqueue an immutable source-reference event.
    pub(crate) async fn enqueue_event(
        &self,
        event: GuardianEvent,
        available_at_unix_secs: i64,
    ) -> Result<bool, GuardianCurationError> {
        validate_event(&event, available_at_unix_secs)?;
        let workspace_json = encode(&event.owner.workspace_ids.iter().collect::<Vec<_>>())?;
        let evidence_json = encode(&event.evidence)?;
        let event_kind = event_kind_value(event.kind);
        let user_id = event.owner.user_id.map(|id| id.0);
        let agent_id = event.owner.agent_id.0;
        let event_id = event.event_id;
        let payload_digest = event.payload_digest;
        let occurred_at = event.occurred_at_unix_secs;
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            let changed = transaction
                .execute(
                    "INSERT OR IGNORE INTO guardian_outbox(event_id,event_kind,owner_user_id,owner_agent_id,workspace_ids_json,evidence_json,payload_digest,occurred_at,available_at,state,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'pending',?8)",
                    params![event_id, event_kind, user_id, agent_id, workspace_json, evidence_json, payload_digest, occurred_at, available_at_unix_secs],
                )
                .map_err(storage_error)?;
            if changed == 0 {
                let matches: i64 = transaction
                    .query_row(
                        "SELECT COUNT(*) FROM guardian_outbox WHERE event_id=?1 AND event_kind=?2 AND owner_user_id IS ?3 AND owner_agent_id=?4 AND workspace_ids_json=?5 AND evidence_json=?6 AND payload_digest=?7 AND occurred_at=?8",
                        params![event_id, event_kind, user_id, agent_id, workspace_json, evidence_json, payload_digest, occurred_at],
                        |row| row.get(0),
                    )
                    .map_err(storage_error)?;
                if matches != 1 {
                    return Err(GuardianCurationError::IdempotencyConflict);
                }
            }
            audit(
                &transaction,
                occurred_at,
                Some(&event_id),
                None,
                None,
                None,
                "runtime",
                "event_enqueued",
                None,
                Some("pending"),
                if changed == 1 { "created" } else { "replayed" },
                Some(&payload_digest),
            )?;
            transaction.commit().map_err(storage_error)?;
            Ok(changed == 1)
        })
        .await
    }

    /// Claim the oldest available event, creating or resuming its idempotent
    /// run. Expired leases are safely reclaimed after a crash.
    pub(crate) async fn claim_next_run(
        &self,
        identity: &GuardianServiceIdentity,
        curator_version: impl Into<String>,
        now_unix_secs: i64,
        lease_secs: i64,
    ) -> Result<Option<ClaimedCuratorRun>, GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let curator_version = curator_version.into();
        validate_text(&curator_version, MAX_VERSION_BYTES)?;
        validate_lease(lease_secs)?;
        let guardian_digest = identity.content_safe_digest();
        let policy_revision = self.policy.revision();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            let candidate = transaction
                .query_row(
                    "SELECT e.event_id,r.run_id,r.attempt,r.state,r.curator_version,r.policy_revision FROM guardian_outbox e LEFT JOIN curator_runs r ON r.event_id=e.event_id WHERE e.state!='completed' AND e.available_at<=?1 AND (r.run_id IS NULL OR r.state='retryable' AND r.next_attempt_at<=?1 OR r.state='running' AND r.lease_expires_at<=?1) ORDER BY e.available_at,e.event_id LIMIT 1",
                    [now_unix_secs],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<i64>>(5)?,
                        ))
                    },
                )
                .optional()
                .map_err(storage_error)?;
            let Some((
                event_id,
                previous_run_id,
                previous_attempt,
                previous_state,
                previous_curator_version,
                previous_policy_revision,
            )) = candidate
            else {
                transaction.commit().map_err(storage_error)?;
                return Ok(None);
            };
            if previous_run_id.is_some()
                && (previous_curator_version.as_deref() != Some(curator_version.as_str())
                    || previous_policy_revision != Some(sql_u64(policy_revision)?))
            {
                return Err(GuardianCurationError::Conflict);
            }
            let run_id = previous_run_id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let attempt = previous_attempt.unwrap_or(0).saturating_add(1);
            let attempt_u32 =
                u32::try_from(attempt).map_err(|_| GuardianCurationError::Corrupt)?;
            let claim_token = Uuid::new_v4().to_string();
            let lease_expires_at = checked_add(now_unix_secs, lease_secs)?;
            let changed = if previous_run_id.is_some() {
                transaction
                    .execute(
                        "UPDATE curator_runs SET guardian_service_digest=?3,state='running',attempt=?4,claim_token=?5,lease_expires_at=?6,updated_at=?7 WHERE run_id=?1 AND event_id=?2 AND (state='retryable' OR state='running' AND lease_expires_at<=?7)",
                        params![run_id, event_id, guardian_digest, attempt, claim_token, lease_expires_at, now_unix_secs],
                    )
                    .map_err(storage_error)?
            } else {
                transaction
                    .execute(
                        "INSERT INTO curator_runs(run_id,event_id,guardian_service_digest,curator_version,policy_revision,state,attempt,claim_token,lease_expires_at,next_attempt_at,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,'running',?6,?7,?8,?9,?9,?9)",
                        params![run_id, event_id, guardian_digest, curator_version, sql_u64(policy_revision)?, attempt, claim_token, lease_expires_at, now_unix_secs],
                    )
                    .map_err(storage_error)?
            };
            ensure_changed(changed)?;
            transaction
                .execute(
                    "UPDATE guardian_outbox SET state='claimed' WHERE event_id=?1",
                    [&event_id],
                )
                .map_err(storage_error)?;
            audit(
                &transaction,
                now_unix_secs,
                Some(&event_id),
                Some(&run_id),
                None,
                None,
                &guardian_digest,
                "run_claimed",
                previous_state.as_deref(),
                Some("running"),
                "lease_acquired",
                None,
            )?;
            transaction.commit().map_err(storage_error)?;
            Ok(Some(ClaimedCuratorRun {
                run_id,
                event_id,
                claim_token,
                attempt: attempt_u32,
                lease_expires_at_unix_secs: lease_expires_at,
                curator_version,
                policy_revision,
            }))
        })
        .await
    }

    /// Extend an active run lease without changing its claim token or versions.
    pub(crate) async fn renew_run(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        now_unix_secs: i64,
        lease_secs: i64,
    ) -> Result<ClaimedCuratorRun, GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        validate_lease(lease_secs)?;
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let expires_at = checked_add(now_unix_secs, lease_secs)?;
            let changed = transaction
                .execute(
                    "UPDATE curator_runs SET lease_expires_at=?3,updated_at=?4 WHERE run_id=?1 AND claim_token=?2 AND state='running'",
                    params![claim.run_id, claim.claim_token, expires_at, now_unix_secs],
                )
                .map_err(storage_error)?;
            if changed != 1 {
                return Err(GuardianCurationError::LeaseLost);
            }
            transaction.commit().map_err(storage_error)?;
            Ok(ClaimedCuratorRun {
                lease_expires_at_unix_secs: expires_at,
                ..claim
            })
        })
        .await
    }

    /// Load the immutable event bound to an active claim.
    pub(crate) async fn event_for_claim(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        now_unix_secs: i64,
    ) -> Result<GuardianEvent, GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let (
                event_kind,
                owner_user_id,
                owner_agent_id,
                workspace_ids_json,
                evidence_json,
                payload_digest,
                occurred_at,
            ) = transaction
                .query_row(
                    "SELECT event_kind,owner_user_id,owner_agent_id,workspace_ids_json,evidence_json,payload_digest,occurred_at FROM guardian_outbox WHERE event_id=?1",
                    [&claim.event_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, i64>(6)?,
                        ))
                    },
                )
                .map_err(storage_error)?;
            let owner = RuntimeOwnerScope::guardian(
                AgentId::new(owner_agent_id),
                owner_user_id.map(UserId::new),
                decode::<Vec<String>>(&workspace_ids_json)?
                    .into_iter()
                    .collect(),
            );
            let event = GuardianEvent::new(
                claim.event_id.clone(),
                parse_event_kind(&event_kind)?,
                owner,
                decode(&evidence_json)?,
                payload_digest,
                occurred_at,
            );
            transaction.commit().map_err(storage_error)?;
            Ok(event)
        })
        .await
    }

    /// Finish a claimed learning run after an owner opts out.
    ///
    /// No candidate or pending mutation may remain executable. Existing
    /// terminal values are not retroactively deleted; explicit correction,
    /// decay, and forget operations remain separate administration paths.
    pub(crate) async fn reject_run_for_learning_opt_out(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        now_unix_secs: i64,
    ) -> Result<(), GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let candidate_ids = {
                let mut statement = transaction
                    .prepare(
                        "SELECT candidate_id FROM memory_candidates
                         WHERE run_id=?1 AND state NOT IN
                         ('duplicate','committed','corrected','decayed','forgotten',
                          'delivery_failed','rejected')
                         ORDER BY candidate_id",
                    )
                    .map_err(storage_error)?;
                statement
                    .query_map([&claim.run_id], |row| row.get::<_, String>(0))
                    .map_err(storage_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(storage_error)?
            };
            for candidate_id in candidate_ids {
                let candidate = load_candidate(&transaction, &candidate_id)?;
                let pending_mutations = {
                    let mut statement = transaction
                        .prepare(
                            "SELECT mutation_id,state FROM guardian_mutation_outbox
                             WHERE candidate_id=?1 AND state IN ('pending','claimed')
                             ORDER BY mutation_id",
                        )
                        .map_err(storage_error)?;
                    statement
                        .query_map([&candidate_id], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })
                        .map_err(storage_error)?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(storage_error)?
                };
                for (mutation_id, from_state) in pending_mutations {
                    let changed = transaction
                        .execute(
                            "UPDATE guardian_mutation_outbox
                             SET state='dead_letter',claim_token=NULL,lease_expires_at=NULL,
                                 last_error_code='learning_opt_out',updated_at=?2,completed_at=?2
                             WHERE mutation_id=?1 AND state IN ('pending','claimed')",
                            params![mutation_id, now_unix_secs],
                        )
                        .map_err(storage_error)?;
                    ensure_changed(changed)?;
                    audit(
                        &transaction,
                        now_unix_secs,
                        Some(&claim.event_id),
                        Some(&claim.run_id),
                        Some(&candidate_id),
                        Some(&mutation_id),
                        &guardian_digest,
                        "mutation_learning_denied",
                        Some(&from_state),
                        Some("dead_letter"),
                        "learning_opt_out",
                        None,
                    )?;
                }
                transition_candidate(
                    &transaction,
                    &candidate,
                    CandidateState::Rejected,
                    None,
                    now_unix_secs,
                )?;
                audit_candidate_transition(
                    &transaction,
                    &claim,
                    &guardian_digest,
                    &candidate_id,
                    now_unix_secs,
                    "candidate_rejected",
                    candidate.state,
                    CandidateState::Rejected,
                    "learning_opt_out",
                    None,
                )?;
            }
            transition_run(
                &transaction,
                &claim,
                "succeeded",
                Some("learning_opt_out"),
                now_unix_secs,
                now_unix_secs,
            )?;
            transaction
                .execute(
                    "UPDATE guardian_outbox SET state='completed',completed_at=?2
                     WHERE event_id=?1 AND state='claimed'",
                    params![claim.event_id, now_unix_secs],
                )
                .map_err(storage_error)
                .and_then(ensure_changed)?;
            audit(
                &transaction,
                now_unix_secs,
                Some(&claim.event_id),
                Some(&claim.run_id),
                None,
                None,
                &guardian_digest,
                "run_completed",
                Some("running"),
                Some("succeeded"),
                "learning_opt_out",
                None,
            )?;
            transaction.commit().map_err(storage_error)
        })
        .await
    }

    /// Release a transiently failed run for bounded future retry.
    pub(crate) async fn retry_run(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        error_code: impl Into<String>,
        now_unix_secs: i64,
        retry_at_unix_secs: i64,
    ) -> Result<(), GuardianCurationError> {
        let error_code = error_code.into();
        validate_reason(&error_code)?;
        validate_retry(now_unix_secs, retry_at_unix_secs)?;
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            transition_run(
                &transaction,
                &claim,
                "retryable",
                Some(&error_code),
                retry_at_unix_secs,
                now_unix_secs,
            )?;
            transaction
                .execute(
                    "UPDATE guardian_outbox SET state='pending',available_at=?2 WHERE event_id=?1",
                    params![claim.event_id, retry_at_unix_secs],
                )
                .map_err(storage_error)?;
            audit(
                &transaction,
                now_unix_secs,
                Some(&claim.event_id),
                Some(&claim.run_id),
                None,
                None,
                &guardian_digest,
                "run_retry_scheduled",
                Some("running"),
                Some("retryable"),
                &error_code,
                None,
            )?;
            transaction.commit().map_err(storage_error)
        })
        .await
    }

    /// Mark an irrecoverable run and its event terminal. Existing candidates
    /// and audit rows remain available for operator inspection.
    pub(crate) async fn fail_run(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        error_code: impl Into<String>,
        now_unix_secs: i64,
    ) -> Result<(), GuardianCurationError> {
        let error_code = error_code.into();
        validate_reason(&error_code)?;
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            transition_run(
                &transaction,
                &claim,
                "failed",
                Some(&error_code),
                now_unix_secs,
                now_unix_secs,
            )?;
            transaction
                .execute(
                    "UPDATE guardian_outbox SET state='failed',completed_at=?2 WHERE event_id=?1",
                    params![claim.event_id, now_unix_secs],
                )
                .map_err(storage_error)?;
            audit(
                &transaction,
                now_unix_secs,
                Some(&claim.event_id),
                Some(&claim.run_id),
                None,
                None,
                &guardian_digest,
                "run_failed",
                Some("running"),
                Some("failed"),
                &error_code,
                None,
            )?;
            transaction.commit().map_err(storage_error)
        })
        .await
    }

    /// Idempotently extract a candidate using its stable source key.
    pub(crate) async fn extract_candidate(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        draft: CandidateDraft,
        now_unix_secs: i64,
    ) -> Result<MemoryCandidate, GuardianCurationError> {
        validate_draft(&draft)?;
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        let source_key = draft.source_key;
        let content_json = encode(&draft.content)?;
        let content_digest = digest_json(&draft.content)?;
        let evidence_json = encode(&draft.evidence)?;
        let origin = origin_value(draft.origin);
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let (owner_user_id, owner_agent_id) = transaction
                .query_row(
                    "SELECT owner_user_id,owner_agent_id FROM guardian_outbox WHERE event_id=?1",
                    [&claim.event_id],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(storage_error)?;
            let candidate_id = Uuid::new_v4().to_string();
            let changed = transaction
                .execute(
                    "INSERT OR IGNORE INTO memory_candidates(candidate_id,run_id,source_key,revision,owner_user_id,owner_agent_id,content_json,content_digest,evidence_json,origin,consent_state,state,created_at,updated_at) VALUES (?1,?2,?3,1,?4,?5,?6,?7,?8,?9,'not_required','extracted',?10,?10)",
                    params![candidate_id, claim.run_id, source_key, owner_user_id, owner_agent_id, content_json, content_digest, evidence_json, origin, now_unix_secs],
                )
                .map_err(storage_error)?;
            let stored = load_candidate_by_source(&transaction, &claim.run_id, &source_key)?;
            if stored.content_digest != content_digest || stored.evidence != draft.evidence {
                return Err(GuardianCurationError::IdempotencyConflict);
            }
            if changed == 1 {
                audit(
                    &transaction,
                    now_unix_secs,
                    Some(&claim.event_id),
                    Some(&claim.run_id),
                    Some(&stored.candidate_id),
                    None,
                    &guardian_digest,
                    "candidate_extracted",
                    None,
                    Some("extracted"),
                    "created",
                    Some(&content_digest),
                )?;
            }
            transaction.commit().map_err(storage_error)?;
            Ok(stored)
        })
        .await
    }

    /// Persist semantic classification while deriving consent and validating
    /// the requested scope against the event's Runtime owner.
    pub(crate) async fn classify_candidate(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        candidate_id: impl Into<String>,
        expected_revision: u64,
        classification: CandidateClassification,
        now_unix_secs: i64,
    ) -> Result<MemoryCandidate, GuardianCurationError> {
        validate_classification(&classification)?;
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let candidate_id = candidate_id.into();
        validate_id(&candidate_id)?;
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let candidate = load_candidate(&transaction, &candidate_id)?;
            ensure_candidate_run(&candidate, &claim)?;
            ensure_revision_state(
                &candidate,
                expected_revision,
                &[CandidateState::Extracted],
            )?;
            validate_classification_owner(&transaction, &claim, &candidate, &classification)?;
            let consent = initial_consent(
                classification.scope,
                classification.sensitivity,
                candidate.origin,
            );
            let expires_at = checked_add(
                now_unix_secs,
                i64::try_from(classification.retention_secs)
                    .map_err(|_| GuardianCurationError::InvalidInput)?,
            )?;
            let next_revision = next_revision(candidate.revision)?;
            let changed = transaction
                .execute(
                    "UPDATE memory_candidates SET revision=?3,scope=?4,workspace_id=?5,confidence_basis_points=?6,sensitivity=?7,consent_state=?8,retention_secs=?9,dedupe_key=?10,state='classified',expires_at=?11,updated_at=?12 WHERE candidate_id=?1 AND revision=?2 AND state='extracted'",
                    params![
                        candidate_id,
                        sql_u64(expected_revision)?,
                        sql_u64(next_revision)?,
                        scope_value(classification.scope),
                        classification.workspace_id,
                        i64::from(classification.confidence_basis_points),
                        sensitivity_value(classification.sensitivity),
                        consent_value(consent),
                        sql_u64(classification.retention_secs)?,
                        classification.dedupe_key,
                        expires_at,
                        now_unix_secs
                    ],
                )
                .map_err(storage_error)?;
            ensure_changed(changed)?;
            audit_candidate_transition(
                &transaction,
                &claim,
                &guardian_digest,
                &candidate_id,
                now_unix_secs,
                "candidate_classified",
                CandidateState::Extracted,
                CandidateState::Classified,
                "classified",
                None,
            )?;
            let stored = load_candidate(&transaction, &candidate_id)?;
            transaction.commit().map_err(storage_error)?;
            Ok(stored)
        })
        .await
    }

    /// Persist duplicate/conflict reconciliation before any policy decision.
    pub(crate) async fn reconcile_candidate(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        candidate_id: impl Into<String>,
        expected_revision: u64,
        reconciliation: Reconciliation,
        now_unix_secs: i64,
    ) -> Result<MemoryCandidate, GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let candidate_id = candidate_id.into();
        validate_id(&candidate_id)?;
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let candidate = load_candidate(&transaction, &candidate_id)?;
            ensure_candidate_run(&candidate, &claim)?;
            ensure_revision_state(
                &candidate,
                expected_revision,
                &[CandidateState::Classified],
            )?;
            let (next_state, conflict_with, reason) = match &reconciliation {
                Reconciliation::Unique => (
                    post_reconcile_state(candidate.consent),
                    None,
                    "unique",
                ),
                Reconciliation::DuplicateOf(existing) => {
                    validate_related_candidate(&transaction, &candidate, existing)?;
                    (CandidateState::Duplicate, Some(existing.as_str()), "duplicate")
                }
                Reconciliation::ConflictWith(existing) => {
                    validate_related_candidate(&transaction, &candidate, existing)?;
                    (CandidateState::Conflict, Some(existing.as_str()), "conflict")
                }
            };
            let next_revision = next_revision(candidate.revision)?;
            let changed = transaction
                .execute(
                    "UPDATE memory_candidates SET revision=?3,conflict_with=?4,state=?5,pending_action=?6,updated_at=?7 WHERE candidate_id=?1 AND revision=?2 AND state='classified'",
                    params![
                        candidate_id,
                        sql_u64(expected_revision)?,
                        sql_u64(next_revision)?,
                        conflict_with,
                        state_value(next_state),
                        if matches!(next_state, CandidateState::PolicyPending) { Some("commit") } else { None },
                        now_unix_secs
                    ],
                )
                .map_err(storage_error)?;
            ensure_changed(changed)?;
            audit_candidate_transition(
                &transaction,
                &claim,
                &guardian_digest,
                &candidate_id,
                now_unix_secs,
                "candidate_reconciled",
                CandidateState::Classified,
                next_state,
                reason,
                None,
            )?;
            let stored = load_candidate(&transaction, &candidate_id)?;
            transaction.commit().map_err(storage_error)?;
            Ok(stored)
        })
        .await
    }

    /// Resolve a same-owner conflict without allowing the curator to reference
    /// or reveal another owner's candidate.
    #[allow(dead_code)] // invoked by the authenticated conflict-resolution surface
    pub(crate) async fn resolve_conflict(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        candidate_id: impl Into<String>,
        expected_revision: u64,
        resolution: ConflictResolution,
        now_unix_secs: i64,
    ) -> Result<MemoryCandidate, GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let candidate_id = candidate_id.into();
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let candidate = load_candidate(&transaction, &candidate_id)?;
            ensure_candidate_run(&candidate, &claim)?;
            ensure_revision_state(&candidate, expected_revision, &[CandidateState::Conflict])?;
            let (next_state, reason) = match resolution {
                ConflictResolution::KeepCandidate => {
                    (post_reconcile_state(candidate.consent), "keep_candidate")
                }
                ConflictResolution::KeepExisting => (CandidateState::Rejected, "keep_existing"),
            };
            transition_candidate(
                &transaction,
                &candidate,
                next_state,
                if matches!(next_state, CandidateState::PolicyPending) {
                    Some(MutationAction::Commit)
                } else {
                    None
                },
                now_unix_secs,
            )?;
            audit_candidate_transition(
                &transaction,
                &claim,
                &guardian_digest,
                &candidate_id,
                now_unix_secs,
                "candidate_conflict_resolved",
                CandidateState::Conflict,
                next_state,
                reason,
                None,
            )?;
            let stored = load_candidate(&transaction, &candidate_id)?;
            transaction.commit().map_err(storage_error)?;
            Ok(stored)
        })
        .await
    }

    /// Apply an explicit confirmation result and advance or reject the
    /// candidate. A classifier cannot call this by setting a field.
    #[allow(dead_code)] // invoked by the authenticated confirmation surface
    pub(crate) async fn confirm_candidate(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        candidate_id: impl Into<String>,
        expected_revision: u64,
        confirmed: bool,
        now_unix_secs: i64,
    ) -> Result<MemoryCandidate, GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let candidate_id = candidate_id.into();
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let candidate = load_candidate(&transaction, &candidate_id)?;
            ensure_candidate_run(&candidate, &claim)?;
            ensure_revision_state(
                &candidate,
                expected_revision,
                &[CandidateState::AwaitingConfirmation],
            )?;
            let next_state = if confirmed {
                CandidateState::PolicyPending
            } else {
                CandidateState::Rejected
            };
            let next_revision = next_revision(candidate.revision)?;
            let changed = transaction
                .execute(
                    "UPDATE memory_candidates SET revision=?3,consent_state=?4,state=?5,pending_action=?6,updated_at=?7 WHERE candidate_id=?1 AND revision=?2 AND state='awaiting_confirmation'",
                    params![
                        candidate_id,
                        sql_u64(expected_revision)?,
                        sql_u64(next_revision)?,
                        if confirmed { "confirmed" } else { "denied" },
                        state_value(next_state),
                        if confirmed { Some("commit") } else { None },
                        now_unix_secs
                    ],
                )
                .map_err(storage_error)?;
            ensure_changed(changed)?;
            audit_candidate_transition(
                &transaction,
                &claim,
                &guardian_digest,
                &candidate_id,
                now_unix_secs,
                "candidate_confirmation",
                CandidateState::AwaitingConfirmation,
                next_state,
                if confirmed { "confirmed" } else { "denied" },
                None,
            )?;
            let stored = load_candidate(&transaction, &candidate_id)?;
            transaction.commit().map_err(storage_error)?;
            Ok(stored)
        })
        .await
    }

    /// Suspend the originating run until an authenticated confirmation event
    /// advances its only active candidate.
    pub(crate) async fn wait_for_confirmation(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        candidate_id: &str,
        now_unix_secs: i64,
    ) -> Result<(), GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let claim = claim.clone();
        let candidate_id = candidate_id.to_owned();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let candidate = load_candidate(&transaction, &candidate_id)?;
            ensure_candidate_run(&candidate, &claim)?;
            if candidate.state != CandidateState::AwaitingConfirmation {
                return Err(GuardianCurationError::Conflict);
            }
            let active_count: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM memory_candidates WHERE run_id=?1 AND state NOT IN ('duplicate','committed','corrected','decayed','forgotten','delivery_failed','rejected')",
                    [&claim.run_id],
                    |row| row.get(0),
                )
                .map_err(storage_error)?;
            if active_count != 1 {
                return Err(GuardianCurationError::Conflict);
            }
            let changed = transaction
                .execute(
                    "UPDATE curator_runs SET state='waiting',claim_token=NULL,lease_expires_at=NULL,updated_at=?2 WHERE run_id=?1 AND state='running'",
                    params![claim.run_id, now_unix_secs],
                )
                .map_err(storage_error)?;
            ensure_changed(changed)?;
            audit(
                &transaction,
                now_unix_secs,
                Some(&claim.event_id),
                Some(&claim.run_id),
                Some(&candidate_id),
                None,
                &guardian_digest,
                "confirmation_requested",
                Some("running"),
                Some("waiting"),
                "user_confirmation_required",
                None,
            )?;
            transaction.commit().map_err(storage_error)
        })
        .await
    }

    /// Apply a confirmation from a distinct authenticated outbox event and
    /// resume the candidate's suspended originating run.
    pub(crate) async fn confirm_from_event(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        candidate_id: &str,
        expected_revision: u64,
        confirmed: bool,
        now_unix_secs: i64,
    ) -> Result<(), GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let claim = claim.clone();
        let candidate_id = candidate_id.to_owned();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let candidate = load_candidate(&transaction, &candidate_id)?;
            ensure_revision_state(
                &candidate,
                expected_revision,
                &[CandidateState::AwaitingConfirmation],
            )?;
            let (event_user, event_agent, event_workspaces) = transaction
                .query_row(
                    "SELECT owner_user_id,owner_agent_id,workspace_ids_json FROM guardian_outbox WHERE event_id=?1",
                    [&claim.event_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(storage_error)?;
            let event_workspaces = decode::<Vec<String>>(&event_workspaces)?;
            if event_agent != candidate.owner_agent_id.0
                || event_user.as_deref()
                    != candidate.owner_user_id.as_ref().map(|id| id.0.as_str())
                || candidate
                    .workspace_id
                    .as_ref()
                    .is_some_and(|workspace| !event_workspaces.contains(workspace))
            {
                return Err(GuardianCurationError::AccessDenied);
            }
            let next_state = if confirmed {
                CandidateState::PolicyPending
            } else {
                CandidateState::Rejected
            };
            let next_revision = next_revision(candidate.revision)?;
            let changed = transaction
                .execute(
                    "UPDATE memory_candidates SET revision=?3,consent_state=?4,state=?5,pending_action=?6,updated_at=?7 WHERE candidate_id=?1 AND revision=?2 AND state='awaiting_confirmation'",
                    params![
                        candidate_id,
                        sql_u64(expected_revision)?,
                        sql_u64(next_revision)?,
                        if confirmed { "confirmed" } else { "denied" },
                        state_value(next_state),
                        if confirmed { Some("commit") } else { None },
                        now_unix_secs
                    ],
                )
                .map_err(storage_error)?;
            ensure_changed(changed)?;
            let resumed = transaction
                .execute(
                    "UPDATE curator_runs SET state='retryable',next_attempt_at=?2,updated_at=?2 WHERE run_id=?1 AND state='waiting'",
                    params![candidate.run_id, now_unix_secs],
                )
                .map_err(storage_error)?;
            ensure_changed(resumed)?;
            transaction
                .execute(
                    "UPDATE guardian_outbox SET state='pending' WHERE event_id=(SELECT event_id FROM curator_runs WHERE run_id=?1)",
                    [&candidate.run_id],
                )
                .map_err(storage_error)?;
            audit(
                &transaction,
                now_unix_secs,
                Some(&claim.event_id),
                Some(&claim.run_id),
                Some(&candidate_id),
                None,
                &guardian_digest,
                "candidate_confirmation",
                Some("awaiting_confirmation"),
                Some(state_value(next_state)),
                if confirmed { "confirmed" } else { "denied" },
                None,
            )?;
            transaction.commit().map_err(storage_error)
        })
        .await
    }

    /// Evaluate the fixed policy revision. Classifier output is input to this
    /// decision and can never substitute for it.
    pub(crate) async fn evaluate_policy(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        candidate_id: impl Into<String>,
        expected_revision: u64,
        now_unix_secs: i64,
    ) -> Result<PolicyDecision, GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let candidate_id = candidate_id.into();
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        let policy = self.policy.clone();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let candidate = load_candidate(&transaction, &candidate_id)?;
            ensure_candidate_run(&candidate, &claim)?;
            ensure_revision_state(
                &candidate,
                expected_revision,
                &[CandidateState::PolicyPending],
            )?;
            let action = candidate
                .pending_action
                .ok_or(GuardianCurationError::Corrupt)?;
            let (outcome, reason_code) = policy.evaluate(&candidate, action);
            let decision_id = Uuid::new_v4().to_string();
            let authorized_revision = next_revision(candidate.revision)?;
            transaction
                .execute(
                    "INSERT INTO guardian_policy_decisions(decision_id,candidate_id,candidate_revision,action,policy_revision,outcome,reason_code,occurred_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        decision_id,
                        candidate_id,
                        sql_u64(authorized_revision)?,
                        action_value(action),
                        sql_u64(policy.revision())?,
                        policy_outcome_value(outcome),
                        reason_code,
                        now_unix_secs
                    ],
                )
                .map_err(storage_error)?;
            let next_state = match outcome {
                PolicyOutcome::Allow => CandidateState::Authorized,
                PolicyOutcome::Deny => CandidateState::Rejected,
            };
            transition_candidate(
                &transaction,
                &candidate,
                next_state,
                Some(action),
                now_unix_secs,
            )?;
            audit_candidate_transition(
                &transaction,
                &claim,
                &guardian_digest,
                &candidate_id,
                now_unix_secs,
                "policy_evaluated",
                CandidateState::PolicyPending,
                next_state,
                reason_code,
                None,
            )?;
            transaction.commit().map_err(storage_error)?;
            Ok(PolicyDecision {
                decision_id,
                candidate_id,
                candidate_revision: authorized_revision,
                action,
                policy_revision: policy.revision(),
                outcome,
                reason_code: reason_code.into(),
            })
        })
        .await
    }

    /// Persist an authorized, idempotent mutation. The owning store applies it
    /// using `idempotency_key`; no conversational session participates.
    pub(crate) async fn schedule_mutation(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        candidate_id: impl Into<String>,
        expected_revision: u64,
        now_unix_secs: i64,
    ) -> Result<String, GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let candidate_id = candidate_id.into();
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        let policy_revision = self.policy.revision();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let candidate = load_candidate(&transaction, &candidate_id)?;
            ensure_candidate_run(&candidate, &claim)?;
            ensure_revision_state(
                &candidate,
                expected_revision,
                &[CandidateState::Authorized],
            )?;
            let action = candidate
                .pending_action
                .ok_or(GuardianCurationError::Corrupt)?;
            let Some(scope) = candidate.scope else {
                return Err(GuardianCurationError::Corrupt);
            };
            let decision_count: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM guardian_policy_decisions WHERE candidate_id=?1 AND candidate_revision=?2 AND action=?3 AND policy_revision=?4 AND outcome='allow'",
                    params![candidate_id, sql_u64(candidate.revision)?, action_value(action), sql_u64(policy_revision)?],
                    |row| row.get(0),
                )
                .map_err(storage_error)?;
            if decision_count != 1 {
                return Err(GuardianCurationError::PolicyDenied);
            }
            transition_candidate(
                &transaction,
                &candidate,
                CandidateState::CommitPending,
                Some(action),
                now_unix_secs,
            )?;
            let scheduled = load_candidate(&transaction, &candidate_id)?;
            let body = mutation_body(&scheduled, action);
            let body_json = encode(&body)?;
            let body_digest = digest_json(&body)?;
            let idempotency_key = mutation_idempotency_key(&scheduled, action, policy_revision);
            let mutation_id = Uuid::new_v4().to_string();
            transaction
                .execute(
                    "INSERT INTO guardian_mutation_outbox(mutation_id,candidate_id,candidate_revision,action,scope,owner_user_id,owner_agent_id,workspace_id,body_json,body_digest,idempotency_key,state,attempt,available_at,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending',0,?12,?12,?12)",
                    params![
                        mutation_id,
                        candidate_id,
                        sql_u64(scheduled.revision)?,
                        action_value(action),
                        scope_value(scope),
                        scheduled.owner_user_id.as_ref().map(|id| id.0.as_str()),
                        scheduled.owner_agent_id.0,
                        scheduled.workspace_id,
                        body_json,
                        body_digest,
                        idempotency_key,
                        now_unix_secs
                    ],
                )
                .map_err(storage_error)?;
            audit_candidate_transition(
                &transaction,
                &claim,
                &guardian_digest,
                &candidate_id,
                now_unix_secs,
                "mutation_scheduled",
                CandidateState::Authorized,
                CandidateState::CommitPending,
                "policy_authorized",
                Some(&mutation_id),
            )?;
            transaction.commit().map_err(storage_error)?;
            Ok(mutation_id)
        })
        .await
    }

    /// Lease the oldest due authorized mutation. Expired delivery leases are
    /// reclaimed with the same idempotency key.
    pub(crate) async fn claim_next_mutation(
        &self,
        identity: &GuardianServiceIdentity,
        now_unix_secs: i64,
        lease_secs: i64,
    ) -> Result<Option<ClaimedMutation>, GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        validate_lease(lease_secs)?;
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            let mutation_id = transaction
                .query_row(
                    "SELECT mutation_id FROM guardian_mutation_outbox WHERE available_at<=?1 AND (state='pending' OR state='claimed' AND lease_expires_at<=?1) ORDER BY available_at,mutation_id LIMIT 1",
                    [now_unix_secs],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(storage_error)?;
            let Some(mutation_id) = mutation_id else {
                transaction.commit().map_err(storage_error)?;
                return Ok(None);
            };
            let claim_token = Uuid::new_v4().to_string();
            let lease_expires_at = checked_add(now_unix_secs, lease_secs)?;
            let changed = transaction
                .execute(
                    "UPDATE guardian_mutation_outbox SET state='claimed',attempt=attempt+1,claim_token=?2,lease_expires_at=?3,updated_at=?4 WHERE mutation_id=?1 AND (state='pending' OR state='claimed' AND lease_expires_at<=?4)",
                    params![mutation_id, claim_token, lease_expires_at, now_unix_secs],
                )
                .map_err(storage_error)?;
            ensure_changed(changed)?;
            let claimed = load_claimed_mutation(
                &transaction,
                &mutation_id,
                &claim_token,
                lease_expires_at,
            )?;
            audit(
                &transaction,
                now_unix_secs,
                None,
                None,
                Some(&claimed.candidate_id),
                Some(&mutation_id),
                &guardian_digest,
                "mutation_claimed",
                Some("pending"),
                Some("claimed"),
                "lease_acquired",
                None,
            )?;
            transaction.commit().map_err(storage_error)?;
            Ok(Some(claimed))
        })
        .await
    }

    /// Acknowledge that the owning store applied the idempotency key, then
    /// advance the candidate in the same local transaction.
    pub(crate) async fn acknowledge_mutation(
        &self,
        identity: &GuardianServiceIdentity,
        mutation: &ClaimedMutation,
        now_unix_secs: i64,
    ) -> Result<MemoryCandidate, GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let mutation = mutation.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_mutation_claim(&transaction, &mutation, now_unix_secs)?;
            let candidate = load_candidate(&transaction, &mutation.candidate_id)?;
            if candidate.revision != mutation.candidate_revision
                || candidate.state != CandidateState::CommitPending
                || candidate.pending_action != Some(mutation.action)
            {
                return Err(GuardianCurationError::Conflict);
            }
            let terminal = action_terminal_state(mutation.action);
            transaction
                .execute(
                    "UPDATE guardian_mutation_outbox SET state='completed',claim_token=NULL,lease_expires_at=NULL,updated_at=?3,completed_at=?3 WHERE mutation_id=?1 AND claim_token=?2 AND state='claimed'",
                    params![mutation.mutation_id, mutation.claim_token, now_unix_secs],
                )
                .map_err(storage_error)?;
            transition_candidate(&transaction, &candidate, terminal, None, now_unix_secs)?;
            audit(
                &transaction,
                now_unix_secs,
                None,
                Some(&candidate.run_id),
                Some(&candidate.candidate_id),
                Some(&mutation.mutation_id),
                &guardian_digest,
                "mutation_completed",
                Some("commit_pending"),
                Some(state_value(terminal)),
                action_value(mutation.action),
                Some(&mutation.idempotency_key),
            )?;
            let stored = load_candidate(&transaction, &candidate.candidate_id)?;
            transaction.commit().map_err(storage_error)?;
            Ok(stored)
        })
        .await
    }

    /// Release a retryable delivery or dead-letter a permanent failure.
    pub(crate) async fn fail_mutation(
        &self,
        identity: &GuardianServiceIdentity,
        mutation: &ClaimedMutation,
        error_code: impl Into<String>,
        now_unix_secs: i64,
        retry_at_unix_secs: Option<i64>,
    ) -> Result<(), GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let error_code = error_code.into();
        validate_reason(&error_code)?;
        if let Some(retry_at) = retry_at_unix_secs {
            validate_retry(now_unix_secs, retry_at)?;
        }
        let mutation = mutation.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_mutation_claim(&transaction, &mutation, now_unix_secs)?;
            let (state, available_at) = retry_at_unix_secs
                .map_or(("dead_letter", now_unix_secs), |retry_at| ("pending", retry_at));
            let changed = transaction
                .execute(
                    "UPDATE guardian_mutation_outbox SET state=?3,available_at=?4,claim_token=NULL,lease_expires_at=NULL,last_error_code=?5,updated_at=?6 WHERE mutation_id=?1 AND claim_token=?2 AND state='claimed'",
                    params![mutation.mutation_id, mutation.claim_token, state, available_at, error_code, now_unix_secs],
                )
                .map_err(storage_error)?;
            ensure_changed(changed)?;
            if retry_at_unix_secs.is_none() {
                let candidate = load_candidate(&transaction, &mutation.candidate_id)?;
                transition_candidate(
                    &transaction,
                    &candidate,
                    CandidateState::DeliveryFailed,
                    None,
                    now_unix_secs,
                )?;
            }
            audit(
                &transaction,
                now_unix_secs,
                None,
                None,
                Some(&mutation.candidate_id),
                Some(&mutation.mutation_id),
                &guardian_digest,
                "mutation_delivery_failed",
                Some("claimed"),
                Some(state),
                &error_code,
                None,
            )?;
            transaction.commit().map_err(storage_error)
        })
        .await
    }

    /// Start a correction cycle. The corrected value must pass a new policy
    /// decision and a new idempotent mutation delivery.
    #[allow(dead_code)] // invoked by the correction administration surface
    pub(crate) async fn correct_candidate(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        candidate_id: impl Into<String>,
        expected_revision: u64,
        correction: CandidateCorrection,
        now_unix_secs: i64,
    ) -> Result<MemoryCandidate, GuardianCurationError> {
        validate_content_evidence(&correction.content, &correction.evidence)?;
        self.request_followup(
            identity,
            claim,
            FollowupRequest {
                candidate_id: candidate_id.into(),
                expected_revision,
                action: MutationAction::Correct,
                replacement: Some((correction.content, correction.evidence)),
            },
            now_unix_secs,
        )
        .await
    }

    /// Start a policy-authorized decay cycle for an existing committed value.
    #[allow(dead_code)] // invoked by the retention administration surface
    pub(crate) async fn decay_candidate(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        candidate_id: impl Into<String>,
        expected_revision: u64,
        now_unix_secs: i64,
    ) -> Result<MemoryCandidate, GuardianCurationError> {
        self.request_followup(
            identity,
            claim,
            FollowupRequest {
                candidate_id: candidate_id.into(),
                expected_revision,
                action: MutationAction::Decay,
                replacement: None,
            },
            now_unix_secs,
        )
        .await
    }

    /// Start a policy-authorized deletion cycle for an existing governed value.
    #[allow(dead_code)] // invoked by the forget administration surface
    pub(crate) async fn forget_candidate(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        candidate_id: impl Into<String>,
        expected_revision: u64,
        now_unix_secs: i64,
    ) -> Result<MemoryCandidate, GuardianCurationError> {
        self.request_followup(
            identity,
            claim,
            FollowupRequest {
                candidate_id: candidate_id.into(),
                expected_revision,
                action: MutationAction::Forget,
                replacement: None,
            },
            now_unix_secs,
        )
        .await
    }

    #[allow(dead_code)] // shared implementation for governed follow-up actions
    async fn request_followup(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        request: FollowupRequest,
        now_unix_secs: i64,
    ) -> Result<MemoryCandidate, GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let FollowupRequest {
                candidate_id,
                expected_revision,
                action,
                replacement,
            } = request;
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let candidate = load_candidate(&transaction, &candidate_id)?;
            ensure_candidate_run(&candidate, &claim)?;
            ensure_revision_state(
                &candidate,
                expected_revision,
                match action {
                    MutationAction::Correct | MutationAction::Decay => {
                        &[CandidateState::Committed, CandidateState::Corrected]
                    }
                    MutationAction::Forget => &[
                        CandidateState::Committed,
                        CandidateState::Corrected,
                        CandidateState::Decayed,
                    ],
                    MutationAction::Commit => return Err(GuardianCurationError::InvalidInput),
                },
            )?;
            let next_revision = next_revision(candidate.revision)?;
            let (content_json, content_digest, evidence_json) =
                if let Some((content, evidence)) = replacement {
                    (
                        encode(&content)?,
                        digest_json(&content)?,
                        encode(&evidence)?,
                    )
                } else {
                    (
                        encode(&candidate.content)?,
                        candidate.content_digest.clone(),
                        encode(&candidate.evidence)?,
                    )
                };
            let changed = transaction
                .execute(
                    "UPDATE memory_candidates SET revision=?3,content_json=?4,content_digest=?5,evidence_json=?6,state='policy_pending',pending_action=?7,updated_at=?8 WHERE candidate_id=?1 AND revision=?2",
                    params![candidate_id, sql_u64(expected_revision)?, sql_u64(next_revision)?, content_json, content_digest, evidence_json, action_value(action), now_unix_secs],
                )
                .map_err(storage_error)?;
            ensure_changed(changed)?;
            audit_candidate_transition(
                &transaction,
                &claim,
                &guardian_digest,
                &candidate_id,
                now_unix_secs,
                match action {
                    MutationAction::Correct => "correction_requested",
                    MutationAction::Decay => "decay_requested",
                    MutationAction::Forget => "forget_requested",
                    MutationAction::Commit => return Err(GuardianCurationError::InvalidInput),
                },
                candidate.state,
                CandidateState::PolicyPending,
                action_value(action),
                None,
            )?;
            let stored = load_candidate(&transaction, &candidate_id)?;
            transaction.commit().map_err(storage_error)?;
            Ok(stored)
        })
        .await
    }

    /// Complete a run only when every candidate is terminal and every
    /// scheduled mutation has a durable terminal delivery state.
    pub(crate) async fn finalize_run(
        &self,
        identity: &GuardianServiceIdentity,
        claim: &ClaimedCuratorRun,
        now_unix_secs: i64,
    ) -> Result<(), GuardianCurationError> {
        identity
            .authorize(&self.guardian_identity, now_unix_secs)
            .map_err(|_| GuardianCurationError::AccessDenied)?;
        let claim = claim.clone();
        let guardian_digest = identity.content_safe_digest();
        self.run(move |connection| {
            let transaction = immediate(connection)?;
            ensure_claim(&transaction, &claim, now_unix_secs, &guardian_digest)?;
            let active_candidates: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM memory_candidates WHERE run_id=?1 AND state NOT IN ('duplicate','committed','corrected','decayed','forgotten','delivery_failed','rejected')",
                    [&claim.run_id],
                    |row| row.get(0),
                )
                .map_err(storage_error)?;
            let active_mutations: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM guardian_mutation_outbox m JOIN memory_candidates c ON c.candidate_id=m.candidate_id WHERE c.run_id=?1 AND m.state IN ('pending','claimed')",
                    [&claim.run_id],
                    |row| row.get(0),
                )
                .map_err(storage_error)?;
            if active_candidates != 0 || active_mutations != 0 {
                return Err(GuardianCurationError::Conflict);
            }
            let permanent_failures: i64 = transaction
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM memory_candidates WHERE run_id=?1 AND state='delivery_failed') + (SELECT COUNT(*) FROM guardian_mutation_outbox m JOIN memory_candidates c ON c.candidate_id=m.candidate_id WHERE c.run_id=?1 AND m.state='dead_letter')",
                    [&claim.run_id],
                    |row| row.get(0),
                )
                .map_err(storage_error)?;
            let (run_state, event_state, outcome) = if permanent_failures == 0 {
                ("succeeded", "completed", "completed")
            } else {
                ("failed", "failed", "delivery_failed")
            };
            transition_run(
                &transaction,
                &claim,
                run_state,
                Some(outcome),
                now_unix_secs,
                now_unix_secs,
            )?;
            transaction
                .execute(
                    "UPDATE guardian_outbox SET state=?2,completed_at=?3 WHERE event_id=?1",
                    params![claim.event_id, event_state, now_unix_secs],
                )
                .map_err(storage_error)?;
            audit(
                &transaction,
                now_unix_secs,
                Some(&claim.event_id),
                Some(&claim.run_id),
                None,
                None,
                &guardian_digest,
                if permanent_failures == 0 {
                    "run_completed"
                } else {
                    "run_failed"
                },
                Some("running"),
                Some(run_state),
                outcome,
                None,
            )?;
            transaction.commit().map_err(storage_error)
        })
        .await
    }

    /// Load a candidate head without exposing whether a denied owner has a
    /// similarly named record through distinct error text.
    pub(crate) async fn candidate(
        &self,
        candidate_id: impl Into<String>,
    ) -> Result<MemoryCandidate, GuardianCurationError> {
        let candidate_id = candidate_id.into();
        self.run(move |connection| load_candidate(connection, &candidate_id))
            .await
    }

    /// Return pending confirmations for one Runtime-derived owner.
    ///
    /// The query accepts no session selector because origin-session binding is
    /// verified against the separately staged payload by `GuardianRuntime`.
    pub(crate) async fn pending_confirmations(
        &self,
        owner_agent_id: sylvander_protocol::AgentId,
        owner_user_id: sylvander_protocol::UserId,
    ) -> Result<Vec<MemoryCandidate>, GuardianCurationError> {
        validate_id(&owner_agent_id.0)?;
        validate_id(&owner_user_id.0)?;
        self.run(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT candidate_id FROM memory_candidates WHERE owner_agent_id=?1 AND owner_user_id=?2 AND state='awaiting_confirmation' ORDER BY created_at,candidate_id",
                )
                .map_err(storage_error)?;
            let ids = statement
                .query_map(params![owner_agent_id.0, owner_user_id.0], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(storage_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(storage_error)?;
            ids.into_iter()
                .map(|candidate_id| load_candidate(connection, &candidate_id))
                .collect()
        })
        .await
    }

    /// Read the typed terminal/retry state used by supervision.
    pub(crate) async fn run_state(
        &self,
        run_id: impl Into<String>,
    ) -> Result<CuratorRunState, GuardianCurationError> {
        let run_id = run_id.into();
        validate_id(&run_id)?;
        self.run(move |connection| {
            let value = connection
                .query_row(
                    "SELECT state FROM curator_runs WHERE run_id=?1",
                    [&run_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(storage_error)?
                .ok_or(GuardianCurationError::AccessDenied)?;
            parse_run_state(&value)
        })
        .await
    }

    /// Read the typed mutation delivery state used by sink supervision.
    pub(crate) async fn mutation_state(
        &self,
        mutation_id: impl Into<String>,
    ) -> Result<MutationDeliveryState, GuardianCurationError> {
        let mutation_id = mutation_id.into();
        validate_id(&mutation_id)?;
        self.run(move |connection| {
            let value = connection
                .query_row(
                    "SELECT state FROM guardian_mutation_outbox WHERE mutation_id=?1",
                    [&mutation_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(storage_error)?
                .ok_or(GuardianCurationError::AccessDenied)?;
            parse_mutation_state(&value)
        })
        .await
    }
}

impl CapabilityAuditSink for GuardianCurationStore {
    fn record(&self, record: &CapabilityAuditRecord) -> Result<(), ()> {
        let connection = self.connection.lock().map_err(|_| ())?;
        connection
            .execute(
                "INSERT INTO capability_invocation_audit(invocation_id,phase,actor,capability,capability_revision,policy_revision,owner_digest,outcome) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    record.invocation_id,
                    capability_audit_phase(record.phase),
                    capability_actor_value(record.actor),
                    record.capability,
                    record.capability_revision,
                    i64::try_from(record.policy_revision).map_err(|_| ())?,
                    record.owner_digest,
                    capability_audit_outcome(record.outcome),
                ],
            )
            .map(|_| ())
            .map_err(|_| ())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum GuardianCurationError {
    #[error("curation input is invalid")]
    InvalidInput,
    #[error("curation access denied")]
    AccessDenied,
    #[error("curation lease was lost")]
    LeaseLost,
    #[error("curation state conflict")]
    Conflict,
    #[error("curation idempotency conflict")]
    IdempotencyConflict,
    #[error("curation policy denied the mutation")]
    PolicyDenied,
    #[error("curation database schema is incompatible")]
    IncompatibleSchema,
    #[error("curation data is corrupt")]
    Corrupt,
    #[error("curation storage failed")]
    Storage,
    #[error("curation storage task failed")]
    Task,
}

fn immediate(connection: &mut Connection) -> Result<Transaction<'_>, GuardianCurationError> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)
}

pub(super) fn storage_error(_: rusqlite::Error) -> GuardianCurationError {
    GuardianCurationError::Storage
}

fn validate_event(event: &GuardianEvent, available_at: i64) -> Result<(), GuardianCurationError> {
    validate_id(&event.event_id)?;
    validate_id(&event.owner.agent_id.0)?;
    if let Some(user_id) = &event.owner.user_id {
        validate_id(&user_id.0)?;
    }
    if event.owner.session_id.is_some()
        || event.owner.workspace_ids.len() > MAX_WORKSPACE_IDS
        || event.occurred_at_unix_secs <= 0
        || available_at < event.occurred_at_unix_secs
        || !valid_digest(&event.payload_digest)
    {
        return Err(GuardianCurationError::InvalidInput);
    }
    validate_evidence(&event.evidence)?;
    for workspace_id in &event.owner.workspace_ids {
        validate_id(workspace_id)?;
    }
    Ok(())
}

fn validate_draft(draft: &CandidateDraft) -> Result<(), GuardianCurationError> {
    validate_text(&draft.source_key, MAX_ID_BYTES)?;
    validate_content_evidence(&draft.content, &draft.evidence)
}

fn validate_content_evidence(
    content: &Value,
    evidence: &[EvidenceReference],
) -> Result<(), GuardianCurationError> {
    let encoded = serde_json::to_vec(content).map_err(|_| GuardianCurationError::InvalidInput)?;
    if encoded.is_empty() || encoded.len() > MAX_CONTENT_BYTES {
        return Err(GuardianCurationError::InvalidInput);
    }
    validate_evidence(evidence)
}

fn validate_evidence(evidence: &[EvidenceReference]) -> Result<(), GuardianCurationError> {
    if evidence.is_empty() || evidence.len() > MAX_EVIDENCE_REFERENCES {
        return Err(GuardianCurationError::InvalidInput);
    }
    for reference in evidence {
        validate_text(&reference.kind, MAX_REFERENCE_BYTES)?;
        validate_text(&reference.reference, MAX_REFERENCE_BYTES)?;
        if !valid_digest(&reference.digest) {
            return Err(GuardianCurationError::InvalidInput);
        }
    }
    Ok(())
}

fn validate_classification(
    classification: &CandidateClassification,
) -> Result<(), GuardianCurationError> {
    if classification.confidence_basis_points > 10_000
        || classification.retention_secs == 0
        || classification.retention_secs > MAX_RETENTION_SECS
    {
        return Err(GuardianCurationError::InvalidInput);
    }
    validate_text(&classification.dedupe_key, MAX_ID_BYTES)?;
    match (classification.scope, &classification.workspace_id) {
        (CandidateScope::WorkspaceKnowledge, Some(workspace_id)) => validate_id(workspace_id),
        (CandidateScope::WorkspaceKnowledge, None) | (_, Some(_)) => {
            Err(GuardianCurationError::InvalidInput)
        }
        (_, None) => Ok(()),
    }
}

fn validate_classification_owner(
    transaction: &Transaction<'_>,
    claim: &ClaimedCuratorRun,
    candidate: &MemoryCandidate,
    classification: &CandidateClassification,
) -> Result<(), GuardianCurationError> {
    if matches!(
        classification.scope,
        CandidateScope::Relationship | CandidateScope::UserProfile
    ) && candidate.owner_user_id.is_none()
    {
        return Err(GuardianCurationError::AccessDenied);
    }
    if let Some(workspace_id) = &classification.workspace_id {
        let workspace_json: String = transaction
            .query_row(
                "SELECT workspace_ids_json FROM guardian_outbox WHERE event_id=?1",
                [&claim.event_id],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        let allowed: Vec<String> = decode(&workspace_json)?;
        if !allowed.contains(workspace_id) {
            return Err(GuardianCurationError::AccessDenied);
        }
    }
    Ok(())
}

fn validate_related_candidate(
    transaction: &Transaction<'_>,
    candidate: &MemoryCandidate,
    related_id: &str,
) -> Result<(), GuardianCurationError> {
    validate_id(related_id)?;
    if related_id == candidate.candidate_id {
        return Err(GuardianCurationError::InvalidInput);
    }
    let related =
        load_candidate(transaction, related_id).map_err(|_| GuardianCurationError::AccessDenied)?;
    if related.owner_agent_id != candidate.owner_agent_id
        || related.owner_user_id != candidate.owner_user_id
        || related.scope != candidate.scope
        || related.workspace_id != candidate.workspace_id
    {
        return Err(GuardianCurationError::AccessDenied);
    }
    Ok(())
}

fn validate_lease(lease_secs: i64) -> Result<(), GuardianCurationError> {
    if !(1..=MAX_LEASE_SECS).contains(&lease_secs) {
        return Err(GuardianCurationError::InvalidInput);
    }
    Ok(())
}

fn validate_retry(now: i64, retry_at: i64) -> Result<(), GuardianCurationError> {
    if retry_at <= now || retry_at > checked_add(now, MAX_RETRY_DELAY_SECS)? {
        return Err(GuardianCurationError::InvalidInput);
    }
    Ok(())
}

fn validate_id(value: &str) -> Result<(), GuardianCurationError> {
    validate_text(value, MAX_ID_BYTES)
}

fn validate_reason(value: &str) -> Result<(), GuardianCurationError> {
    validate_text(value, MAX_REASON_BYTES)?;
    if value
        .bytes()
        .any(|byte| !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)))
    {
        return Err(GuardianCurationError::InvalidInput);
    }
    Ok(())
}

fn validate_text(value: &str, max_bytes: usize) -> Result<(), GuardianCurationError> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(GuardianCurationError::InvalidInput);
    }
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn ensure_claim(
    transaction: &Transaction<'_>,
    claim: &ClaimedCuratorRun,
    now: i64,
    guardian_digest: &str,
) -> Result<(), GuardianCurationError> {
    let valid: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM curator_runs WHERE run_id=?1 AND event_id=?2 AND claim_token=?3 AND state='running' AND lease_expires_at>?4 AND guardian_service_digest=?5 AND curator_version=?6 AND policy_revision=?7",
            params![claim.run_id, claim.event_id, claim.claim_token, now, guardian_digest, claim.curator_version, sql_u64(claim.policy_revision)?],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if valid != 1 {
        return Err(GuardianCurationError::LeaseLost);
    }
    Ok(())
}

fn ensure_mutation_claim(
    transaction: &Transaction<'_>,
    mutation: &ClaimedMutation,
    now: i64,
) -> Result<(), GuardianCurationError> {
    let valid: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM guardian_mutation_outbox WHERE mutation_id=?1 AND claim_token=?2 AND state='claimed' AND lease_expires_at>?3 AND candidate_id=?4 AND candidate_revision=?5 AND action=?6 AND idempotency_key=?7",
            params![mutation.mutation_id, mutation.claim_token, now, mutation.candidate_id, sql_u64(mutation.candidate_revision)?, action_value(mutation.action), mutation.idempotency_key],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if valid != 1 {
        return Err(GuardianCurationError::LeaseLost);
    }
    Ok(())
}

fn ensure_candidate_run(
    candidate: &MemoryCandidate,
    claim: &ClaimedCuratorRun,
) -> Result<(), GuardianCurationError> {
    if candidate.run_id != claim.run_id {
        return Err(GuardianCurationError::AccessDenied);
    }
    Ok(())
}

fn ensure_revision_state(
    candidate: &MemoryCandidate,
    revision: u64,
    states: &[CandidateState],
) -> Result<(), GuardianCurationError> {
    if candidate.revision != revision || !states.contains(&candidate.state) {
        return Err(GuardianCurationError::Conflict);
    }
    Ok(())
}

fn ensure_changed(changed: usize) -> Result<(), GuardianCurationError> {
    if changed != 1 {
        return Err(GuardianCurationError::Conflict);
    }
    Ok(())
}

fn transition_run(
    transaction: &Transaction<'_>,
    claim: &ClaimedCuratorRun,
    state: &str,
    outcome: Option<&str>,
    next_attempt_at: i64,
    now: i64,
) -> Result<(), GuardianCurationError> {
    let changed = transaction
        .execute(
            "UPDATE curator_runs SET state=?3,claim_token=NULL,lease_expires_at=NULL,next_attempt_at=?4,outcome_code=?5,updated_at=?6 WHERE run_id=?1 AND claim_token=?2 AND state='running'",
            params![claim.run_id, claim.claim_token, state, next_attempt_at, outcome, now],
        )
        .map_err(storage_error)?;
    ensure_changed(changed)
}

fn transition_candidate(
    transaction: &Transaction<'_>,
    candidate: &MemoryCandidate,
    state: CandidateState,
    action: Option<MutationAction>,
    now: i64,
) -> Result<(), GuardianCurationError> {
    let changed = transaction
        .execute(
            "UPDATE memory_candidates SET revision=?3,state=?4,pending_action=?5,updated_at=?6 WHERE candidate_id=?1 AND revision=?2",
            params![
                candidate.candidate_id,
                sql_u64(candidate.revision)?,
                sql_u64(next_revision(candidate.revision)?)?,
                state_value(state),
                action.map(action_value),
                now
            ],
        )
        .map_err(storage_error)?;
    ensure_changed(changed)
}

#[allow(clippy::too_many_arguments)]
fn audit_candidate_transition(
    transaction: &Transaction<'_>,
    claim: &ClaimedCuratorRun,
    guardian_digest: &str,
    candidate_id: &str,
    now: i64,
    operation: &str,
    from: CandidateState,
    to: CandidateState,
    reason: &str,
    mutation_id: Option<&str>,
) -> Result<(), GuardianCurationError> {
    audit(
        transaction,
        now,
        Some(&claim.event_id),
        Some(&claim.run_id),
        Some(candidate_id),
        mutation_id,
        guardian_digest,
        operation,
        Some(state_value(from)),
        Some(state_value(to)),
        reason,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn audit(
    transaction: &Transaction<'_>,
    occurred_at: i64,
    event_id: Option<&str>,
    run_id: Option<&str>,
    candidate_id: Option<&str>,
    mutation_id: Option<&str>,
    guardian_service_digest: &str,
    operation: &str,
    from_state: Option<&str>,
    to_state: Option<&str>,
    reason_code: &str,
    record_digest: Option<&str>,
) -> Result<(), GuardianCurationError> {
    transaction
        .execute(
            "INSERT INTO guardian_curation_audit(audit_id,occurred_at,event_id,run_id,candidate_id,mutation_id,guardian_service_digest,operation,from_state,to_state,reason_code,record_digest) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![Uuid::new_v4().to_string(), occurred_at, event_id, run_id, candidate_id, mutation_id, guardian_service_digest, operation, from_state, to_state, reason_code, record_digest],
        )
        .map(|_| ())
        .map_err(storage_error)
}

fn load_candidate(
    connection: &Connection,
    candidate_id: &str,
) -> Result<MemoryCandidate, GuardianCurationError> {
    connection
        .query_row(
            "SELECT candidate_id,run_id,revision,scope,owner_user_id,owner_agent_id,workspace_id,content_json,content_digest,evidence_json,confidence_basis_points,origin,sensitivity,consent_state,retention_secs,dedupe_key,conflict_with,state,pending_action,expires_at FROM memory_candidates WHERE candidate_id=?1",
            [candidate_id],
            decode_candidate,
        )
        .optional()
        .map_err(storage_error)?
        .ok_or(GuardianCurationError::AccessDenied)
}

fn load_candidate_by_source(
    connection: &Connection,
    run_id: &str,
    source_key: &str,
) -> Result<MemoryCandidate, GuardianCurationError> {
    connection
        .query_row(
            "SELECT candidate_id,run_id,revision,scope,owner_user_id,owner_agent_id,workspace_id,content_json,content_digest,evidence_json,confidence_basis_points,origin,sensitivity,consent_state,retention_secs,dedupe_key,conflict_with,state,pending_action,expires_at FROM memory_candidates WHERE run_id=?1 AND source_key=?2",
            params![run_id, source_key],
            decode_candidate,
        )
        .map_err(storage_error)
}

fn decode_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryCandidate> {
    let revision = row.get::<_, i64>(2)?;
    let content_json = row.get::<_, String>(7)?;
    let evidence_json = row.get::<_, String>(9)?;
    let confidence = row.get::<_, Option<i64>>(10)?;
    let retention = row.get::<_, Option<i64>>(14)?;
    Ok(MemoryCandidate {
        candidate_id: row.get(0)?,
        run_id: row.get(1)?,
        revision: u64::try_from(revision).map_err(|_| corrupt_sql(2))?,
        scope: row
            .get::<_, Option<String>>(3)?
            .map(|value| parse_scope(&value, 3))
            .transpose()?,
        owner_user_id: row.get::<_, Option<String>>(4)?.map(UserId),
        owner_agent_id: AgentId(row.get(5)?),
        workspace_id: row.get(6)?,
        content: serde_json::from_str(&content_json).map_err(|_| corrupt_sql(7))?,
        content_digest: row.get(8)?,
        evidence: serde_json::from_str(&evidence_json).map_err(|_| corrupt_sql(9))?,
        confidence_basis_points: confidence
            .map(|value| u16::try_from(value).map_err(|_| corrupt_sql(10)))
            .transpose()?,
        origin: parse_origin(&row.get::<_, String>(11)?, 11)?,
        sensitivity: row
            .get::<_, Option<String>>(12)?
            .map(|value| parse_sensitivity(&value, 12))
            .transpose()?,
        consent: parse_consent(&row.get::<_, String>(13)?, 13)?,
        retention_secs: retention
            .map(|value| u64::try_from(value).map_err(|_| corrupt_sql(14)))
            .transpose()?,
        dedupe_key: row.get(15)?,
        conflict_with: row.get(16)?,
        state: parse_state(&row.get::<_, String>(17)?, 17)?,
        pending_action: row
            .get::<_, Option<String>>(18)?
            .map(|value| parse_action(&value, 18))
            .transpose()?,
        expires_at_unix_secs: row.get(19)?,
    })
}

fn load_claimed_mutation(
    connection: &Connection,
    mutation_id: &str,
    claim_token: &str,
    lease_expires_at: i64,
) -> Result<ClaimedMutation, GuardianCurationError> {
    connection
        .query_row(
            "SELECT mutation_id,candidate_id,candidate_revision,action,scope,owner_user_id,owner_agent_id,workspace_id,body_json,idempotency_key,attempt FROM guardian_mutation_outbox WHERE mutation_id=?1 AND claim_token=?2 AND state='claimed'",
            params![mutation_id, claim_token],
            |row| {
                let revision = u64::try_from(row.get::<_, i64>(2)?)
                    .map_err(|_| corrupt_sql(2))?;
                let body_json = row.get::<_, String>(8)?;
                let attempt =
                    u32::try_from(row.get::<_, i64>(10)?).map_err(|_| corrupt_sql(10))?;
                Ok(ClaimedMutation {
                    mutation_id: row.get(0)?,
                    candidate_id: row.get(1)?,
                    candidate_revision: revision,
                    action: parse_action(&row.get::<_, String>(3)?, 3)?,
                    scope: parse_scope(&row.get::<_, String>(4)?, 4)?,
                    owner_user_id: row.get::<_, Option<String>>(5)?.map(UserId),
                    owner_agent_id: AgentId(row.get(6)?),
                    workspace_id: row.get(7)?,
                    body: serde_json::from_str(&body_json).map_err(|_| corrupt_sql(8))?,
                    idempotency_key: row.get(9)?,
                    claim_token: claim_token.into(),
                    attempt,
                    lease_expires_at_unix_secs: lease_expires_at,
                })
            },
        )
        .map_err(storage_error)
}

fn mutation_body(candidate: &MemoryCandidate, action: MutationAction) -> Value {
    let mut body = json!({
        "action": action_value(action),
        "candidate_id": candidate.candidate_id,
        "candidate_revision": candidate.revision,
        "content_digest": candidate.content_digest,
        "expires_at_unix_secs": candidate.expires_at_unix_secs,
        "retention_secs": candidate.retention_secs,
    });
    if matches!(action, MutationAction::Commit | MutationAction::Correct) {
        body["content"] = candidate.content.clone();
    }
    body
}

fn mutation_idempotency_key(
    candidate: &MemoryCandidate,
    action: MutationAction,
    policy_revision: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sylvander.guardian.mutation.v1\0");
    hasher.update(candidate.candidate_id.as_bytes());
    hasher.update(candidate.revision.to_be_bytes());
    hasher.update(action_value(action).as_bytes());
    hasher.update(policy_revision.to_be_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn digest_json(value: &Value) -> Result<String, GuardianCurationError> {
    let encoded = serde_json::to_vec(value).map_err(|_| GuardianCurationError::InvalidInput)?;
    let mut hasher = Sha256::new();
    hasher.update(b"sylvander.guardian.content.v1\0");
    hasher.update(encoded);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, GuardianCurationError> {
    serde_json::to_string(value).map_err(|_| GuardianCurationError::InvalidInput)
}

fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, GuardianCurationError> {
    serde_json::from_str(value).map_err(|_| GuardianCurationError::Corrupt)
}

fn checked_add(left: i64, right: i64) -> Result<i64, GuardianCurationError> {
    left.checked_add(right)
        .ok_or(GuardianCurationError::InvalidInput)
}

fn next_revision(revision: u64) -> Result<u64, GuardianCurationError> {
    revision
        .checked_add(1)
        .ok_or(GuardianCurationError::Conflict)
}

fn sql_u64(value: u64) -> Result<i64, GuardianCurationError> {
    i64::try_from(value).map_err(|_| GuardianCurationError::InvalidInput)
}

fn post_reconcile_state(consent: ConsentState) -> CandidateState {
    if matches!(consent, ConsentState::Pending) {
        CandidateState::AwaitingConfirmation
    } else {
        CandidateState::PolicyPending
    }
}

fn initial_consent(
    scope: CandidateScope,
    sensitivity: Sensitivity,
    _origin: CandidateOrigin,
) -> ConsentState {
    if matches!(scope, CandidateScope::UserProfile)
        || matches!(sensitivity, Sensitivity::Personal | Sensitivity::Secret)
    {
        ConsentState::Pending
    } else {
        ConsentState::NotRequired
    }
}

fn action_terminal_state(action: MutationAction) -> CandidateState {
    match action {
        MutationAction::Commit => CandidateState::Committed,
        MutationAction::Correct => CandidateState::Corrected,
        MutationAction::Decay => CandidateState::Decayed,
        MutationAction::Forget => CandidateState::Forgotten,
    }
}

fn event_kind_value(value: GuardianEventKind) -> &'static str {
    match value {
        GuardianEventKind::SessionClosed => "session_closed",
        GuardianEventKind::MemoryCandidateCreated => "memory_candidate_created",
        GuardianEventKind::UserFeedbackReceived => "user_feedback_received",
        GuardianEventKind::UserConfirmationReceived => "user_confirmation_received",
        GuardianEventKind::RetentionSweep => "retention_sweep",
    }
}

fn parse_event_kind(value: &str) -> Result<GuardianEventKind, GuardianCurationError> {
    match value {
        "session_closed" => Ok(GuardianEventKind::SessionClosed),
        "memory_candidate_created" => Ok(GuardianEventKind::MemoryCandidateCreated),
        "user_feedback_received" => Ok(GuardianEventKind::UserFeedbackReceived),
        "user_confirmation_received" => Ok(GuardianEventKind::UserConfirmationReceived),
        "retention_sweep" => Ok(GuardianEventKind::RetentionSweep),
        _ => Err(GuardianCurationError::Corrupt),
    }
}

fn scope_value(value: CandidateScope) -> &'static str {
    match value {
        CandidateScope::Relationship => "relationship",
        CandidateScope::UserProfile => "user_profile",
        CandidateScope::AgentCanonical => "agent_canonical",
        CandidateScope::WorkspaceKnowledge => "workspace_knowledge",
    }
}

fn parse_scope(value: &str, index: usize) -> rusqlite::Result<CandidateScope> {
    match value {
        "relationship" => Ok(CandidateScope::Relationship),
        "user_profile" => Ok(CandidateScope::UserProfile),
        "agent_canonical" => Ok(CandidateScope::AgentCanonical),
        "workspace_knowledge" => Ok(CandidateScope::WorkspaceKnowledge),
        _ => Err(corrupt_sql(index)),
    }
}

fn origin_value(value: CandidateOrigin) -> &'static str {
    match value {
        CandidateOrigin::Explicit => "explicit",
        CandidateOrigin::Inferred => "inferred",
    }
}

fn parse_origin(value: &str, index: usize) -> rusqlite::Result<CandidateOrigin> {
    match value {
        "explicit" => Ok(CandidateOrigin::Explicit),
        "inferred" => Ok(CandidateOrigin::Inferred),
        _ => Err(corrupt_sql(index)),
    }
}

fn sensitivity_value(value: Sensitivity) -> &'static str {
    match value {
        Sensitivity::Public => "public",
        Sensitivity::Internal => "internal",
        Sensitivity::Personal => "personal",
        Sensitivity::Secret => "secret",
    }
}

fn parse_sensitivity(value: &str, index: usize) -> rusqlite::Result<Sensitivity> {
    match value {
        "public" => Ok(Sensitivity::Public),
        "internal" => Ok(Sensitivity::Internal),
        "personal" => Ok(Sensitivity::Personal),
        "secret" => Ok(Sensitivity::Secret),
        _ => Err(corrupt_sql(index)),
    }
}

fn consent_value(value: ConsentState) -> &'static str {
    match value {
        ConsentState::NotRequired => "not_required",
        ConsentState::Pending => "pending",
        ConsentState::Confirmed => "confirmed",
        ConsentState::Denied => "denied",
    }
}

fn parse_consent(value: &str, index: usize) -> rusqlite::Result<ConsentState> {
    match value {
        "not_required" => Ok(ConsentState::NotRequired),
        "pending" => Ok(ConsentState::Pending),
        "confirmed" => Ok(ConsentState::Confirmed),
        "denied" => Ok(ConsentState::Denied),
        _ => Err(corrupt_sql(index)),
    }
}

fn action_value(value: MutationAction) -> &'static str {
    match value {
        MutationAction::Commit => "commit",
        MutationAction::Correct => "correct",
        MutationAction::Decay => "decay",
        MutationAction::Forget => "forget",
    }
}

fn parse_action(value: &str, index: usize) -> rusqlite::Result<MutationAction> {
    match value {
        "commit" => Ok(MutationAction::Commit),
        "correct" => Ok(MutationAction::Correct),
        "decay" => Ok(MutationAction::Decay),
        "forget" => Ok(MutationAction::Forget),
        _ => Err(corrupt_sql(index)),
    }
}

fn state_value(value: CandidateState) -> &'static str {
    match value {
        CandidateState::Extracted => "extracted",
        CandidateState::Classified => "classified",
        CandidateState::Duplicate => "duplicate",
        CandidateState::Conflict => "conflict",
        CandidateState::AwaitingConfirmation => "awaiting_confirmation",
        CandidateState::PolicyPending => "policy_pending",
        CandidateState::Authorized => "authorized",
        CandidateState::CommitPending => "commit_pending",
        CandidateState::Committed => "committed",
        CandidateState::Corrected => "corrected",
        CandidateState::Decayed => "decayed",
        CandidateState::Forgotten => "forgotten",
        CandidateState::DeliveryFailed => "delivery_failed",
        CandidateState::Rejected => "rejected",
    }
}

fn parse_state(value: &str, index: usize) -> rusqlite::Result<CandidateState> {
    match value {
        "extracted" => Ok(CandidateState::Extracted),
        "classified" => Ok(CandidateState::Classified),
        "duplicate" => Ok(CandidateState::Duplicate),
        "conflict" => Ok(CandidateState::Conflict),
        "awaiting_confirmation" => Ok(CandidateState::AwaitingConfirmation),
        "policy_pending" => Ok(CandidateState::PolicyPending),
        "authorized" => Ok(CandidateState::Authorized),
        "commit_pending" => Ok(CandidateState::CommitPending),
        "committed" => Ok(CandidateState::Committed),
        "corrected" => Ok(CandidateState::Corrected),
        "decayed" => Ok(CandidateState::Decayed),
        "forgotten" => Ok(CandidateState::Forgotten),
        "delivery_failed" => Ok(CandidateState::DeliveryFailed),
        "rejected" => Ok(CandidateState::Rejected),
        _ => Err(corrupt_sql(index)),
    }
}

fn parse_run_state(value: &str) -> Result<CuratorRunState, GuardianCurationError> {
    match value {
        "running" => Ok(CuratorRunState::Running),
        "waiting" => Ok(CuratorRunState::Waiting),
        "retryable" => Ok(CuratorRunState::Retryable),
        "succeeded" => Ok(CuratorRunState::Succeeded),
        "failed" => Ok(CuratorRunState::Failed),
        _ => Err(GuardianCurationError::Corrupt),
    }
}

fn parse_mutation_state(value: &str) -> Result<MutationDeliveryState, GuardianCurationError> {
    match value {
        "pending" => Ok(MutationDeliveryState::Pending),
        "claimed" => Ok(MutationDeliveryState::Claimed),
        "completed" => Ok(MutationDeliveryState::Completed),
        "dead_letter" => Ok(MutationDeliveryState::DeadLetter),
        _ => Err(GuardianCurationError::Corrupt),
    }
}

fn policy_outcome_value(value: PolicyOutcome) -> &'static str {
    match value {
        PolicyOutcome::Allow => "allow",
        PolicyOutcome::Deny => "deny",
    }
}

fn capability_actor_value(value: CapabilityActor) -> &'static str {
    match value {
        CapabilityActor::Worker => "worker",
        CapabilityActor::Guardian => "guardian",
    }
}

fn capability_audit_phase(value: CapabilityAuditPhase) -> &'static str {
    match value {
        CapabilityAuditPhase::Authorized => "authorized",
        CapabilityAuditPhase::Completed => "completed",
    }
}

fn capability_audit_outcome(value: CapabilityAuditOutcome) -> &'static str {
    match value {
        CapabilityAuditOutcome::Allowed => "allowed",
        CapabilityAuditOutcome::Succeeded => "succeeded",
        CapabilityAuditOutcome::Failed => "failed",
    }
}

fn corrupt_sql(index: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid curation value",
        )),
    )
}

#[cfg(test)]
#[path = "../tests/unit/guardian_curation.rs"]
mod tests;
