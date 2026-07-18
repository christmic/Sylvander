use serde::{Deserialize, Serialize};
use serde_json::Value;
use sylvander_protocol::{AgentId, UserId};

use crate::capability_runtime::RuntimeOwnerScope;

pub(crate) const MAX_CONTENT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_EVIDENCE_REFERENCES: usize = 64;
pub(crate) const MAX_REFERENCE_BYTES: usize = 2 * 1024;
pub(crate) const MAX_RETENTION_SECS: u64 = 5 * 365 * 24 * 60 * 60;
pub(crate) const MAX_WORKSPACE_IDS: usize = 32;

/// Runtime events that can start or resume curation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GuardianEventKind {
    SessionClosed,
    MemoryCandidateCreated,
    UserFeedbackReceived,
    UserConfirmationReceived,
    RetentionSweep,
}

/// Immutable reference to source evidence; content remains in its owning
/// evidence/artifact store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EvidenceReference {
    pub(crate) kind: String,
    pub(crate) reference: String,
    pub(crate) digest: String,
}

/// An outbox event contains immutable references, not copied transcripts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuardianEvent {
    pub(crate) event_id: String,
    pub(crate) kind: GuardianEventKind,
    pub(crate) owner: RuntimeOwnerScope,
    pub(crate) evidence: Vec<EvidenceReference>,
    pub(crate) payload_digest: String,
    pub(crate) occurred_at_unix_secs: i64,
}

impl GuardianEvent {
    pub(crate) fn new(
        event_id: impl Into<String>,
        kind: GuardianEventKind,
        owner: RuntimeOwnerScope,
        evidence: Vec<EvidenceReference>,
        payload_digest: impl Into<String>,
        occurred_at_unix_secs: i64,
    ) -> Self {
        Self {
            event_id: event_id.into(),
            kind,
            owner,
            evidence,
            payload_digest: payload_digest.into(),
            occurred_at_unix_secs,
        }
    }
}

/// Retryable state of one idempotent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CuratorRunState {
    Running,
    Waiting,
    Retryable,
    Succeeded,
    Failed,
}

/// Lease returned only after the Guardian service identity is validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimedCuratorRun {
    pub(crate) run_id: String,
    pub(crate) event_id: String,
    pub(crate) claim_token: String,
    pub(crate) attempt: u32,
    pub(crate) lease_expires_at_unix_secs: i64,
    pub(crate) curator_version: String,
    pub(crate) policy_revision: u64,
}

/// Governed storage target selected during classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CandidateScope {
    Relationship,
    UserProfile,
    AgentCanonical,
    WorkspaceKnowledge,
}

/// Whether the observation was stated directly or inferred semantically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CandidateOrigin {
    Explicit,
    Inferred,
}

/// Data-handling class used by deterministic policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Sensitivity {
    Public,
    Internal,
    Personal,
    Secret,
}

/// Confirmation is a Runtime transition and cannot be asserted by a
/// classifier response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConsentState {
    NotRequired,
    Pending,
    Confirmed,
    Denied,
}

/// Persisted candidate state. Transitions are compare-and-swap guarded by the
/// candidate revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CandidateState {
    Extracted,
    Classified,
    Duplicate,
    Conflict,
    AwaitingConfirmation,
    PolicyPending,
    Authorized,
    CommitPending,
    Committed,
    Corrected,
    Decayed,
    Forgotten,
    DeliveryFailed,
    Rejected,
}

impl CandidateState {
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Duplicate
                | Self::Committed
                | Self::Corrected
                | Self::Decayed
                | Self::Forgotten
                | Self::DeliveryFailed
                | Self::Rejected
        )
    }
}

/// Mutation requested from the owning canonical/profile store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MutationAction {
    Commit,
    Correct,
    Decay,
    Forget,
}

/// Extractor output. Ownership and timestamps are intentionally absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CandidateDraft {
    /// Stable extractor-local key (for example `message:42/preference:0`).
    /// Replaying the same run and key returns the existing candidate.
    pub(crate) source_key: String,
    pub(crate) content: Value,
    pub(crate) evidence: Vec<EvidenceReference>,
    pub(crate) origin: CandidateOrigin,
}

/// Semantic classification fields. Consent and owner are deliberately
/// derived elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CandidateClassification {
    pub(crate) scope: CandidateScope,
    pub(crate) confidence_basis_points: u16,
    pub(crate) sensitivity: Sensitivity,
    pub(crate) retention_secs: u64,
    pub(crate) dedupe_key: String,
    /// Required only for workspace knowledge and must be one of the
    /// Runtime-derived event workspace IDs.
    pub(crate) workspace_id: Option<String>,
}

/// Corrected content plus immutable evidence references.
#[allow(dead_code)] // carried by the authenticated correction surface
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CandidateCorrection {
    pub(crate) content: Value,
    pub(crate) evidence: Vec<EvidenceReference>,
}

/// Result of deterministic duplicate/conflict reconciliation.
#[allow(dead_code)] // duplicate/conflict variants are selected by semantic curation
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reconciliation {
    Unique,
    DuplicateOf(String),
    ConflictWith(String),
}

/// Operator/policy resolution for a same-owner conflict.
#[allow(dead_code)] // consumed by the authenticated conflict-resolution surface
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConflictResolution {
    KeepCandidate,
    KeepExisting,
}

/// Durable candidate head returned to the curator.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MemoryCandidate {
    pub(crate) candidate_id: String,
    pub(crate) run_id: String,
    pub(crate) revision: u64,
    pub(crate) scope: Option<CandidateScope>,
    pub(crate) owner_user_id: Option<UserId>,
    pub(crate) owner_agent_id: AgentId,
    pub(crate) workspace_id: Option<String>,
    pub(crate) content: Value,
    pub(crate) content_digest: String,
    pub(crate) evidence: Vec<EvidenceReference>,
    pub(crate) confidence_basis_points: Option<u16>,
    pub(crate) origin: CandidateOrigin,
    pub(crate) sensitivity: Option<Sensitivity>,
    pub(crate) consent: ConsentState,
    pub(crate) retention_secs: Option<u64>,
    pub(crate) dedupe_key: Option<String>,
    pub(crate) conflict_with: Option<String>,
    pub(crate) state: CandidateState,
    pub(crate) pending_action: Option<MutationAction>,
    pub(crate) expires_at_unix_secs: Option<i64>,
}

/// Binary policy result; the reason code carries the stable explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolicyOutcome {
    Allow,
    Deny,
}

/// Immutable persisted policy decision tied to an exact revision and action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PolicyDecision {
    pub(crate) decision_id: String,
    pub(crate) candidate_id: String,
    pub(crate) candidate_revision: u64,
    pub(crate) action: MutationAction,
    pub(crate) policy_revision: u64,
    pub(crate) outcome: PolicyOutcome,
    pub(crate) reason_code: String,
}

/// Durable mutation-outbox delivery state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MutationDeliveryState {
    Pending,
    Claimed,
    Completed,
    DeadLetter,
}

/// Leased mutation sent to an idempotent owning-store adapter.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClaimedMutation {
    pub(crate) mutation_id: String,
    pub(crate) candidate_id: String,
    pub(crate) candidate_revision: u64,
    pub(crate) action: MutationAction,
    pub(crate) scope: CandidateScope,
    pub(crate) owner_user_id: Option<UserId>,
    pub(crate) owner_agent_id: AgentId,
    pub(crate) workspace_id: Option<String>,
    pub(crate) body: Value,
    pub(crate) idempotency_key: String,
    pub(crate) claim_token: String,
    pub(crate) attempt: u32,
    pub(crate) lease_expires_at_unix_secs: i64,
}
