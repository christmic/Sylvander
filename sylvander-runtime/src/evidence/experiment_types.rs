use serde::{Deserialize, Serialize};
use sylvander_protocol::EvidenceReference;

use super::evaluation_types::EvaluationComparison;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentPhase {
    Baseline,
    Candidate,
    Observation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfChangeExperimentStatus {
    Prepared,
    CandidateEvaluated,
    MergeApproved,
    Merged,
    Observing,
    Completed,
    RollbackRequired,
    RolledBack,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfChangeExperiment {
    pub id: String,
    pub proposal_id: String,
    pub lease_id: String,
    pub branch: String,
    pub base_commit: String,
    pub proposal_state_revision: u64,
    pub started_by_principal_digest: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSelfChangeExperiment {
    pub definition: SelfChangeExperiment,
    pub status: SelfChangeExperimentStatus,
    pub state_revision: u64,
    pub baseline_bundle_id: Option<String>,
    pub candidate_bundle_id: Option<String>,
    pub merge_commit: Option<String>,
    pub rollback_commit: Option<String>,
    pub observation_bundle_id: Option<String>,
    pub merge_approved_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsignedExperimentEvidence {
    pub experiment_id: String,
    pub phase: ExperimentPhase,
    pub proposal_digest_sha256: String,
    pub workspace_commit: String,
    pub evaluations: Vec<EvaluationComparison>,
    pub artifacts: Vec<EvidenceReference>,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedExperimentEvidence {
    pub id: String,
    pub evidence: UnsignedExperimentEvidence,
    pub digest_sha256: String,
    pub signer_key_id: String,
    pub signature_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordExperimentEvidence {
    pub id: String,
    pub expected_state_revision: u64,
    pub principal_digest: String,
    pub evidence: UnsignedExperimentEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentTransition {
    pub experiment_id: String,
    pub expected_state_revision: u64,
    pub principal_digest: String,
    pub reason: Option<String>,
    pub occurred_at: i64,
}
