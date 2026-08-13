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

#[test]
fn executable_plan_rejects_duplicate_cells() {
    let plan = RuntimeBenchPlan {
        schema_version: 1,
        coordinates: vec![coordinate(), coordinate()],
    };
    assert_eq!(plan.validate(), Err(RuntimeBenchError::DuplicateCoordinate));
}

#[test]
fn summary_keeps_safety_recovery_and_cost_independent() {
    let safe = RuntimeBenchResult {
        coordinate: coordinate(),
        verifier_reward: Some(0.5),
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
        messages: 1,
        handoffs: 0,
        moderator_interventions: 0,
        workspace_conflicts: 0,
    };
    let mut unsafe_result = safe.clone();
    unsafe_result.coordinate.run_ordinal = 2;
    unsafe_result.invariant_violations = 1;
    unsafe_result.duplicate_effects = 1;
    unsafe_result.duration_millis = 100;
    let summary = summarize(&[safe, unsafe_result]).unwrap();
    assert_eq!(summary.samples, 2);
    assert_eq!(summary.runtime_safe_basis_points, 5_000);
    assert_eq!(summary.useful_completion_basis_points, 10_000);
    assert_eq!(summary.recovery_basis_points, 10_000);
    assert_eq!(summary.p50_duration_millis, 10);
    assert_eq!(summary.p95_duration_millis, 100);
    assert_eq!(summary.input_tokens, 40);
}
