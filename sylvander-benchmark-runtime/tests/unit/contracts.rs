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
