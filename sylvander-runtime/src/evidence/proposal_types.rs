use sylvander_protocol::EvidenceReference;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImprovementRisk {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImprovementProposalStatus {
    Draft,
    ReadyForReview,
    Approved,
    Rejected,
    Experimenting,
    Completed,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredEvaluation {
    pub dataset_id: String,
    pub dataset_revision: u64,
    pub baseline_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImprovementProposal {
    pub id: String,
    pub cohort_digest_sha256: String,
    pub evidence: Vec<EvidenceReference>,
    pub hypothesis: String,
    pub expected_benefit: String,
    pub risk: ImprovementRisk,
    pub affected_components: Vec<String>,
    pub rollback_plan: String,
    pub required_evaluations: Vec<RequiredEvaluation>,
    pub created_by_principal_digest: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImprovementProposal {
    pub definition: ImprovementProposal,
    pub digest_sha256: String,
    pub status: ImprovementProposalStatus,
    pub state_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalTransition {
    pub proposal_id: String,
    pub expected_state_revision: u64,
    pub status: ImprovementProposalStatus,
    pub principal_digest: String,
    pub reason: Option<String>,
    pub occurred_at: i64,
}
