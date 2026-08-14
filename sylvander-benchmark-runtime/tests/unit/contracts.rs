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
        models: vec![BenchmarkModelBinding {
            role: BenchmarkModelRole::Primary,
            model: "provider/model".into(),
        }],
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
    invalid.models.push(BenchmarkModelBinding {
        role: BenchmarkModelRole::Critic,
        model: "provider/critic".into(),
    });
    assert_eq!(
        invalid.validate(),
        Err(RuntimeBenchError::CognitionModelMismatch)
    );
}

#[test]
fn one_exact_model_can_fill_distinct_internal_roles_without_becoming_two_agents() {
    let mut fast_slow = coordinate();
    fast_slow.cognition = CognitionProfile::FastSlow;
    fast_slow.models.push(BenchmarkModelBinding {
        role: BenchmarkModelRole::Deliberation,
        model: "provider/model".into(),
    });
    fast_slow.validate().unwrap();
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
        primary_model_calls: 1,
        auxiliary_model_calls: 0,
        perception_calls: 0,
        cognitive_fallbacks: 0,
        tool_calls: 1,
        messages: 0,
        handoffs: 0,
        moderator_interventions: 0,
        workspace_conflicts: 0,
        doctor_findings: 0,
        doctor_false_positives: 0,
        doctor_proposals: 0,
        doctor_auto_applied: 0,
    };
    result.validate().unwrap();
    assert!(result.is_runtime_safe());
    assert_eq!(result.verifier_reward, None);
}

#[test]
fn cognition_accounting_and_doctor_authority_are_release_invariants() {
    let mut result = RuntimeBenchResult {
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
        model_calls: 2,
        primary_model_calls: 1,
        auxiliary_model_calls: 0,
        perception_calls: 0,
        cognitive_fallbacks: 0,
        tool_calls: 0,
        messages: 0,
        handoffs: 0,
        moderator_interventions: 0,
        workspace_conflicts: 0,
        doctor_findings: 1,
        doctor_false_positives: 0,
        doctor_proposals: 1,
        doctor_auto_applied: 0,
    };
    assert_eq!(
        result.validate(),
        Err(RuntimeBenchError::InvalidCognitionMetrics)
    );
    result.model_calls = 1;
    result.doctor_auto_applied = 1;
    result.validate().unwrap();
    assert!(!result.is_runtime_safe());
}

#[test]
fn audio_specialist_coordinate_requires_observed_perception_work() {
    let mut result = RuntimeBenchResult {
        coordinate: RuntimeBenchCoordinate {
            cognition: CognitionProfile::PerceptionSpecialist,
            models: vec![
                BenchmarkModelBinding {
                    role: BenchmarkModelRole::Primary,
                    model: "provider/primary".into(),
                },
                BenchmarkModelBinding {
                    role: BenchmarkModelRole::Audio,
                    model: "provider/audio".into(),
                },
            ],
            ..coordinate()
        },
        verifier_reward: None,
        useful_completion: true,
        invariant_violations: 0,
        duplicate_effects: 0,
        user_visible_failures: 0,
        recovered: true,
        duration_millis: 10,
        input_tokens: 20,
        output_tokens: 5,
        model_calls: 2,
        primary_model_calls: 1,
        auxiliary_model_calls: 1,
        perception_calls: 0,
        cognitive_fallbacks: 0,
        tool_calls: 0,
        messages: 1,
        handoffs: 0,
        moderator_interventions: 0,
        workspace_conflicts: 0,
        doctor_findings: 0,
        doctor_false_positives: 0,
        doctor_proposals: 0,
        doctor_auto_applied: 0,
    };
    assert_eq!(
        result.validate(),
        Err(RuntimeBenchError::InvalidCognitionMetrics)
    );
    result.perception_calls = 1;
    result.validate().unwrap();
    result.perception_calls = 3;
    assert_eq!(
        result.validate(),
        Err(RuntimeBenchError::InvalidCognitionMetrics)
    );
}

#[test]
fn executable_plan_rejects_duplicate_cells() {
    let plan = RuntimeBenchPlan {
        schema_version: 2,
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
        primary_model_calls: 1,
        auxiliary_model_calls: 0,
        perception_calls: 0,
        cognitive_fallbacks: 0,
        tool_calls: 1,
        messages: 1,
        handoffs: 0,
        moderator_interventions: 0,
        workspace_conflicts: 0,
        doctor_findings: 0,
        doctor_false_positives: 0,
        doctor_proposals: 0,
        doctor_auto_applied: 0,
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

#[test]
fn canonical_corpus_manifests_pass_schema_and_activation_gate() {
    use crate::{ActivationGateError, ActivationGatePolicy, evaluate_cognition_activation};

    let manifest_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("sylvander-runtime/benchmarks/corpus");
    let policy: ActivationGatePolicy =
        serde_json::from_slice(&std::fs::read(manifest_root.join("policy.json")).expect("policy"))
            .expect("policy decode");

    let corpus_paths = ["corpus-fastslow.json", "corpus-perception.json"];

    for corpus in corpus_paths {
        let manifest_path = manifest_root.join(corpus);
        let manifest = crate::CorpusManifest::from_json(
            &std::fs::read(&manifest_path).expect("manifest bytes"),
        )
        .unwrap_or_else(|error| panic!("{corpus} manifest decode: {error}"));
        manifest
            .verify_artifacts(&manifest_path)
            .unwrap_or_else(|error| panic!("{corpus} artifact verification: {error}"));
        let (baseline_plan, candidate_plan) = manifest
            .paired_plans()
            .unwrap_or_else(|error| panic!("{corpus} paired plans: {error}"));
        let baseline_path = manifest_root.join(corpus.replace(".json", "-baseline.json"));
        let candidate_path = manifest_root.join(corpus.replace(".json", "-candidate.json"));
        let baseline: Vec<RuntimeBenchResult> =
            serde_json::from_slice(&std::fs::read(&baseline_path).expect("baseline bytes"))
                .expect("baseline decode");
        let candidate: Vec<RuntimeBenchResult> =
            serde_json::from_slice(&std::fs::read(&candidate_path).expect("candidate bytes"))
                .expect("candidate decode");
        assert_eq!(
            baseline.len(),
            baseline_plan.coordinates.len(),
            "{corpus} baseline count must match paired plan"
        );
        assert_eq!(
            candidate.len(),
            candidate_plan.coordinates.len(),
            "{corpus} candidate count must match paired plan"
        );
        let report =
            evaluate_cognition_activation(&baseline, &candidate, manifest.candidate, policy)
                .unwrap_or_else(|error: ActivationGateError| {
                    panic!("{corpus} evaluation: {error:?}")
                });
        assert_eq!(
            report.candidate, manifest.candidate,
            "{corpus} report candidate must match manifest candidate"
        );
        assert_eq!(
            report.decision,
            crate::ActivationDecision::Eligible,
            "{corpus} activation gate must be eligible; report={report:?}"
        );
        assert_eq!(
            report.unsafe_candidates, 0,
            "{corpus} no unsafe candidate samples"
        );
        assert_eq!(
            report.quality_win_basis_points, 10_000,
            "{corpus} every pair must improve verifier reward under current policy"
        );
    }
}
