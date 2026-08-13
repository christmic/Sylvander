//! Non-production scenario contracts for evaluating Runtime composition.
//!
//! Production crates never depend on this crate. Harness adapters execute
//! public Runtime boundaries and report evidence against these coordinates.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

mod fault;
mod ledger;

pub use fault::{
    FaultController, FaultDecision, FaultInjectionError, FaultInjectionSpec, FaultReceipt,
};
pub use ledger::{AppendOutcome, BenchmarkLedger, BenchmarkLedgerError, PlanCoverage};

/// Runtime capability family under evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioFamily {
    CrashRecovery,
    MultiAgentCoordination,
    WorkspaceConcurrency,
    CognitiveRouting,
    MultimodalPerception,
    DoctorExperiment,
}

/// Exact durable boundary at which a controlled harness interrupts Runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePoint {
    None,
    TurnPrepared,
    ModelStarted,
    ModelCommitted,
    ToolStarted,
    ToolCommitted,
    PerceptionMediaPersisted,
    PerceptionInferenceStarted,
    PerceptionInferenceCompleted,
    PerceptionArtifactPersisted,
    ResultPersisted,
    MailboxDelivered,
    WorkflowTransitioned,
    WorkspaceMergeStarted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopologyProfile {
    SingleAgent,
    ForkTree,
    PeerMesh,
    ModeratorSwarm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceProfile {
    ReadOnlyShared,
    IsolatedWorktrees,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitionProfile {
    PrimaryOnly,
    FastSlow,
    PrimaryCritic,
    PerceptionSpecialist,
}

/// Responsibility of one model route inside a single Agent. Reusing the same
/// exact model for multiple roles is valid; roles, not model strings, are the
/// unique benchmark dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkModelRole {
    Primary,
    FastDraft,
    Deliberation,
    Critic,
    Vision,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkModelBinding {
    pub role: BenchmarkModelRole,
    pub model: String,
}

/// One immutable benchmark coordinate. Model identities are opaque strings so
/// the harness can compare one- and multi-model cognition without importing a
/// production registry implementation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeBenchCoordinate {
    pub suite: String,
    pub suite_revision: String,
    pub scenario_id: String,
    pub family: ScenarioFamily,
    pub topology: TopologyProfile,
    pub workspace: WorkspaceProfile,
    pub failure_point: FailurePoint,
    pub cognition: CognitionProfile,
    pub models: Vec<BenchmarkModelBinding>,
    pub run_ordinal: u32,
}

impl RuntimeBenchCoordinate {
    pub fn validate(&self) -> Result<(), RuntimeBenchError> {
        if self.suite.trim().is_empty()
            || self.suite_revision.trim().is_empty()
            || self.scenario_id.trim().is_empty()
            || self.models.is_empty()
            || self.run_ordinal == 0
        {
            return Err(RuntimeBenchError::IncompleteCoordinate);
        }
        if self
            .models
            .iter()
            .any(|binding| binding.model.trim().is_empty())
            || self
                .models
                .iter()
                .map(|binding| binding.role)
                .collect::<HashSet<_>>()
                .len()
                != self.models.len()
        {
            return Err(RuntimeBenchError::InvalidModels);
        }
        let roles = self
            .models
            .iter()
            .map(|binding| binding.role)
            .collect::<HashSet<_>>();
        if !roles.contains(&BenchmarkModelRole::Primary)
            || !profile_accepts_roles(self.cognition, &roles)
        {
            return Err(RuntimeBenchError::CognitionModelMismatch);
        }
        if self.family != ScenarioFamily::CrashRecovery && self.failure_point != FailurePoint::None
        {
            return Err(RuntimeBenchError::UnexpectedFailurePoint);
        }
        Ok(())
    }
}

fn profile_accepts_roles(profile: CognitionProfile, roles: &HashSet<BenchmarkModelRole>) -> bool {
    let only = |allowed: &[BenchmarkModelRole]| roles.iter().all(|role| allowed.contains(role));
    match profile {
        CognitionProfile::PrimaryOnly => {
            roles.len() == 1 && roles.contains(&BenchmarkModelRole::Primary)
        }
        CognitionProfile::FastSlow => {
            roles.len() >= 2
                && only(&[
                    BenchmarkModelRole::Primary,
                    BenchmarkModelRole::FastDraft,
                    BenchmarkModelRole::Deliberation,
                ])
                && roles.iter().any(|role| {
                    matches!(
                        role,
                        BenchmarkModelRole::FastDraft | BenchmarkModelRole::Deliberation
                    )
                })
        }
        CognitionProfile::PrimaryCritic => {
            roles.len() == 2 && roles.contains(&BenchmarkModelRole::Critic)
        }
        CognitionProfile::PerceptionSpecialist => {
            roles.len() >= 2
                && only(&[
                    BenchmarkModelRole::Primary,
                    BenchmarkModelRole::Vision,
                    BenchmarkModelRole::Audio,
                ])
                && roles.iter().any(|role| {
                    matches!(role, BenchmarkModelRole::Vision | BenchmarkModelRole::Audio)
                })
        }
    }
}

/// Content-safe outcome. External verifier reward is retained independently
/// from Runtime invariants and user-experience observations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeBenchResult {
    pub coordinate: RuntimeBenchCoordinate,
    pub verifier_reward: Option<f64>,
    pub useful_completion: bool,
    pub invariant_violations: u32,
    pub duplicate_effects: u32,
    pub user_visible_failures: u32,
    pub recovered: bool,
    pub duration_millis: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model_calls: u32,
    pub primary_model_calls: u32,
    pub auxiliary_model_calls: u32,
    pub perception_calls: u32,
    pub cognitive_fallbacks: u32,
    pub tool_calls: u32,
    pub messages: u32,
    pub handoffs: u32,
    pub moderator_interventions: u32,
    pub workspace_conflicts: u32,
    pub doctor_findings: u32,
    pub doctor_false_positives: u32,
    pub doctor_proposals: u32,
    pub doctor_auto_applied: u32,
}

impl RuntimeBenchResult {
    pub fn validate(&self) -> Result<(), RuntimeBenchError> {
        self.coordinate.validate()?;
        if self
            .verifier_reward
            .is_some_and(|reward| !reward.is_finite())
        {
            return Err(RuntimeBenchError::InvalidReward);
        }
        if self.recovered && self.coordinate.failure_point == FailurePoint::None {
            return Err(RuntimeBenchError::SpuriousRecovery);
        }
        if self.primary_model_calls == 0
            || self
                .primary_model_calls
                .checked_add(self.auxiliary_model_calls)
                != Some(self.model_calls)
            || (self.coordinate.cognition == CognitionProfile::PrimaryOnly
                && self.auxiliary_model_calls != 0)
            || self.perception_calls > self.model_calls
            || (self.coordinate.cognition == CognitionProfile::PerceptionSpecialist
                && self.perception_calls == 0)
        {
            return Err(RuntimeBenchError::InvalidCognitionMetrics);
        }
        Ok(())
    }

    /// A release-safety sample requires useful completion and exact-once
    /// invariants. It deliberately does not reinterpret verifier reward.
    #[must_use]
    pub const fn is_runtime_safe(&self) -> bool {
        self.useful_completion
            && self.invariant_violations == 0
            && self.duplicate_effects == 0
            && self.user_visible_failures == 0
            && self.doctor_auto_applied == 0
    }
}

/// Expand exact repetitions without hiding unsupported or failed cells.
pub fn expand_matrix(
    templates: &[RuntimeBenchCoordinate],
    repetitions: u32,
) -> Result<Vec<RuntimeBenchCoordinate>, RuntimeBenchError> {
    if repetitions == 0 {
        return Err(RuntimeBenchError::ZeroRepetitions);
    }
    let mut expanded = Vec::with_capacity(templates.len().saturating_mul(repetitions as usize));
    let mut identities = HashSet::new();
    for template in templates {
        let mut coordinate = template.clone();
        coordinate.run_ordinal = 1;
        coordinate.validate()?;
        for run_ordinal in 1..=repetitions {
            coordinate.run_ordinal = run_ordinal;
            if !identities.insert(coordinate.clone()) {
                return Err(RuntimeBenchError::DuplicateCoordinate);
            }
            expanded.push(coordinate.clone());
        }
    }
    Ok(expanded)
}

/// Versioned executable plan. Keeping the exact coordinates in the artifact
/// prevents a harness from silently dropping expensive or failing cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeBenchPlan {
    pub schema_version: u32,
    pub coordinates: Vec<RuntimeBenchCoordinate>,
}

impl RuntimeBenchPlan {
    pub fn validate(&self) -> Result<(), RuntimeBenchError> {
        if self.schema_version != 2 || self.coordinates.is_empty() {
            return Err(RuntimeBenchError::InvalidPlan);
        }
        let mut identities = HashSet::with_capacity(self.coordinates.len());
        for coordinate in &self.coordinates {
            coordinate.validate()?;
            if !identities.insert(coordinate) {
                return Err(RuntimeBenchError::DuplicateCoordinate);
            }
        }
        Ok(())
    }
}

/// Deterministic aggregate for CI comparisons. Rates use basis points to avoid
/// platform-dependent floating point formatting in evidence artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeBenchSummary {
    pub samples: u64,
    pub runtime_safe_basis_points: u16,
    pub useful_completion_basis_points: u16,
    pub recovery_basis_points: u16,
    pub invariant_violations: u64,
    pub duplicate_effects: u64,
    pub user_visible_failures: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model_calls: u64,
    pub primary_model_calls: u64,
    pub auxiliary_model_calls: u64,
    pub perception_calls: u64,
    pub cognitive_fallbacks: u64,
    pub tool_calls: u64,
    pub doctor_findings: u64,
    pub doctor_false_positives: u64,
    pub doctor_proposals: u64,
    pub doctor_auto_applied: u64,
    pub p50_duration_millis: u64,
    pub p95_duration_millis: u64,
}

pub fn summarize(results: &[RuntimeBenchResult]) -> Result<RuntimeBenchSummary, RuntimeBenchError> {
    if results.is_empty() {
        return Err(RuntimeBenchError::EmptyResults);
    }
    for result in results {
        result.validate()?;
    }
    let samples = u64::try_from(results.len()).map_err(|_| RuntimeBenchError::MetricOverflow)?;
    let mut durations = results
        .iter()
        .map(|result| result.duration_millis)
        .collect::<Vec<_>>();
    durations.sort_unstable();
    Ok(RuntimeBenchSummary {
        samples,
        runtime_safe_basis_points: basis_points(
            results
                .iter()
                .filter(|result| result.is_runtime_safe())
                .count(),
            results.len(),
        )?,
        useful_completion_basis_points: basis_points(
            results
                .iter()
                .filter(|result| result.useful_completion)
                .count(),
            results.len(),
        )?,
        recovery_basis_points: basis_points(
            results.iter().filter(|result| result.recovered).count(),
            results.len(),
        )?,
        invariant_violations: sum_u32(results, |result| result.invariant_violations)?,
        duplicate_effects: sum_u32(results, |result| result.duplicate_effects)?,
        user_visible_failures: sum_u32(results, |result| result.user_visible_failures)?,
        input_tokens: sum_u64(results, |result| result.input_tokens)?,
        output_tokens: sum_u64(results, |result| result.output_tokens)?,
        model_calls: sum_u32(results, |result| result.model_calls)?,
        primary_model_calls: sum_u32(results, |result| result.primary_model_calls)?,
        auxiliary_model_calls: sum_u32(results, |result| result.auxiliary_model_calls)?,
        perception_calls: sum_u32(results, |result| result.perception_calls)?,
        cognitive_fallbacks: sum_u32(results, |result| result.cognitive_fallbacks)?,
        tool_calls: sum_u32(results, |result| result.tool_calls)?,
        doctor_findings: sum_u32(results, |result| result.doctor_findings)?,
        doctor_false_positives: sum_u32(results, |result| result.doctor_false_positives)?,
        doctor_proposals: sum_u32(results, |result| result.doctor_proposals)?,
        doctor_auto_applied: sum_u32(results, |result| result.doctor_auto_applied)?,
        p50_duration_millis: percentile(&durations, 50),
        p95_duration_millis: percentile(&durations, 95),
    })
}

fn basis_points(numerator: usize, denominator: usize) -> Result<u16, RuntimeBenchError> {
    let scaled = numerator
        .checked_mul(10_000)
        .ok_or(RuntimeBenchError::MetricOverflow)?
        / denominator;
    u16::try_from(scaled).map_err(|_| RuntimeBenchError::MetricOverflow)
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let index = sorted
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1);
    sorted[index.min(sorted.len().saturating_sub(1))]
}

fn sum_u32(
    results: &[RuntimeBenchResult],
    project: impl Fn(&RuntimeBenchResult) -> u32,
) -> Result<u64, RuntimeBenchError> {
    results.iter().try_fold(0_u64, |total, result| {
        total
            .checked_add(u64::from(project(result)))
            .ok_or(RuntimeBenchError::MetricOverflow)
    })
}

fn sum_u64(
    results: &[RuntimeBenchResult],
    project: impl Fn(&RuntimeBenchResult) -> u64,
) -> Result<u64, RuntimeBenchError> {
    results.iter().try_fold(0_u64, |total, result| {
        total
            .checked_add(project(result))
            .ok_or(RuntimeBenchError::MetricOverflow)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeBenchError {
    #[error("Runtime benchmark coordinate is incomplete")]
    IncompleteCoordinate,
    #[error("Runtime benchmark model identities are invalid")]
    InvalidModels,
    #[error("cognition profile and model identities do not match")]
    CognitionModelMismatch,
    #[error("cognition call metrics do not match the declared profile")]
    InvalidCognitionMetrics,
    #[error("fault injection belongs only to crash-recovery scenarios")]
    UnexpectedFailurePoint,
    #[error("verifier reward must be finite")]
    InvalidReward,
    #[error("recovery cannot be reported without an injected failure")]
    SpuriousRecovery,
    #[error("benchmark repetitions must be greater than zero")]
    ZeroRepetitions,
    #[error("benchmark matrix contains a duplicate coordinate")]
    DuplicateCoordinate,
    #[error("Runtime benchmark plan is empty or uses an unknown schema")]
    InvalidPlan,
    #[error("Runtime benchmark results are empty")]
    EmptyResults,
    #[error("Runtime benchmark metric overflow")]
    MetricOverflow,
}

#[cfg(test)]
#[path = "../tests/unit/contracts.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/unit/ledger.rs"]
mod ledger_tests;

#[cfg(test)]
#[path = "../tests/unit/fault.rs"]
mod fault_tests;
