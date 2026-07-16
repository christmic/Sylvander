#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisPrivacyScope {
    ShareableOnly,
    IncludePrivate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortQuery {
    pub agent_id: Option<String>,
    pub started_at_inclusive: i64,
    pub started_before_exclusive: i64,
    pub privacy_scope: AnalysisPrivacyScope,
    pub limit: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    None,
    UserReported,
    Tool,
    InteractionTimeout,
    Interrupted,
    RuntimeOrModel,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnalysisWarning {
    MixedAgents,
    IncompleteOutcomes,
    IncompletePricing,
    SparseFeedback,
    MixedFeedbackPrivacy,
    RunLevelFeedbackExcluded,
    LimitReached,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortTurn {
    pub id: String,
    pub run_id: String,
    pub agent_id: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub latency_secs: Option<u64>,
    pub status: String,
    pub successful_outcome: Option<bool>,
    pub failure_class: FailureClass,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub iteration_count: u64,
    pub cost_nano_usd: Option<u64>,
    pub tool_count: u64,
    pub failed_tool_count: u64,
    pub approval_request_count: u64,
    pub approval_decision_count: u64,
    pub retry_count: u64,
    pub timeout_count: u64,
    pub positive_feedback_count: u64,
    pub negative_feedback_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortAnalysis {
    pub cohort_digest_sha256: String,
    pub turns: Vec<CohortTurn>,
    pub succeeded_turns: u64,
    pub failed_turns: u64,
    pub incomplete_turns: u64,
    pub success_rate_basis_points: Option<u16>,
    pub failure_breakdown: FailureBreakdown,
    pub latency_sample_count: u64,
    pub mean_latency_secs: Option<u64>,
    pub p50_latency_secs: Option<u64>,
    pub p95_latency_secs: Option<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub fully_priced_cost_nano_usd: Option<u64>,
    pub tool_count: u64,
    pub failed_tool_count: u64,
    pub approval_request_count: u64,
    pub approval_decision_count: u64,
    pub retry_count: u64,
    pub timeout_count: u64,
    pub positive_feedback_count: u64,
    pub negative_feedback_count: u64,
    pub positive_feedback_rate_basis_points: Option<u16>,
    pub warnings: Vec<AnalysisWarning>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailureBreakdown {
    pub user_reported: u64,
    pub tool: u64,
    pub interaction_timeout: u64,
    pub interrupted: u64,
    pub runtime_or_model: u64,
    pub incomplete: u64,
}
