//! Non-production scenario contracts for evaluating Runtime composition.
//!
//! Production crates never depend on this crate. Harness adapters execute
//! public Runtime boundaries and report evidence against these coordinates.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

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
    pub models: Vec<String>,
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
        if self.models.iter().any(|model| model.trim().is_empty())
            || self.models.iter().collect::<HashSet<_>>().len() != self.models.len()
        {
            return Err(RuntimeBenchError::InvalidModels);
        }
        if self.cognition == CognitionProfile::PrimaryOnly && self.models.len() != 1 {
            return Err(RuntimeBenchError::CognitionModelMismatch);
        }
        if self.family != ScenarioFamily::CrashRecovery && self.failure_point != FailurePoint::None
        {
            return Err(RuntimeBenchError::UnexpectedFailurePoint);
        }
        Ok(())
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
    pub tool_calls: u32,
    pub messages: u32,
    pub handoffs: u32,
    pub moderator_interventions: u32,
    pub workspace_conflicts: u32,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeBenchError {
    #[error("Runtime benchmark coordinate is incomplete")]
    IncompleteCoordinate,
    #[error("Runtime benchmark model identities are invalid")]
    InvalidModels,
    #[error("cognition profile and model identities do not match")]
    CognitionModelMismatch,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coordinate() -> RuntimeBenchCoordinate {
        RuntimeBenchCoordinate {
            suite: "local-faults".into(),
            suite_revision: "v1".into(),
            scenario_id: "tool-effect-started".into(),
            family: ScenarioFamily::CrashRecovery,
            topology: TopologyProfile::SingleAgent,
            workspace: WorkspaceProfile::IsolatedWorktrees,
            failure_point: FailurePoint::ToolStarted,
            cognition: CognitionProfile::PrimaryOnly,
            models: vec!["provider/model".into()],
            run_ordinal: 1,
        }
    }

    #[test]
    fn matrix_retains_each_repetition() {
        let expanded = expand_matrix(&[coordinate()], 3).unwrap();
        assert_eq!(expanded.len(), 3);
        assert_eq!(expanded[2].run_ordinal, 3);
    }

    #[test]
    fn primary_only_rejects_hidden_auxiliary_models() {
        let mut invalid = coordinate();
        invalid.models.push("provider/critic".into());
        assert_eq!(
            invalid.validate(),
            Err(RuntimeBenchError::CognitionModelMismatch)
        );
    }

    #[test]
    fn runtime_safety_never_infers_external_reward() {
        let result = RuntimeBenchResult {
            coordinate: coordinate(),
            verifier_reward: None,
            useful_completion: true,
            invariant_violations: 0,
            duplicate_effects: 0,
            user_visible_failures: 0,
            recovered: true,
            duration_millis: 10,
            input_tokens: 20,
            output_tokens: 5,
            model_calls: 1,
            tool_calls: 1,
            messages: 0,
            handoffs: 0,
            moderator_interventions: 0,
            workspace_conflicts: 0,
        };
        result.validate().unwrap();
        assert!(result.is_runtime_safe());
        assert_eq!(result.verifier_reward, None);
    }
}
