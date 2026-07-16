use sylvander_protocol::EvidenceReference;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationSplit {
    Fixture,
    HeldOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreDirection {
    HigherIsBetter,
    LowerIsBetter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoringAdapterKind {
    BooleanValidation,
    NumericMetric,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoringAdapterRevision {
    pub id: String,
    pub revision: u64,
    pub kind: ScoringAdapterKind,
    pub metric: String,
    /// Digest of the immutable executable/configuration used by the adapter.
    pub config_digest_sha256: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationCase {
    pub id: String,
    pub split: EvaluationSplit,
    pub input: EvidenceReference,
    pub expected: Option<EvidenceReference>,
    pub scorer_id: String,
    pub scorer_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationDatasetRevision {
    pub id: String,
    pub revision: u64,
    pub name: String,
    pub cases: Vec<EvaluationCase>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvaluationDataset {
    pub definition: EvaluationDatasetRevision,
    pub digest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegressionMetric {
    pub metric: String,
    pub direction: ScoreDirection,
    /// Fixed-point value whose unit is defined by the scorer revision.
    pub baseline_value: i64,
    pub sample_count: u64,
    pub max_regression_basis_points: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationBaseline {
    pub id: String,
    pub dataset_id: String,
    pub dataset_revision: u64,
    pub metrics: Vec<RegressionMetric>,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvaluationBaseline {
    pub definition: EvaluationBaseline,
    pub digest_sha256: String,
}
