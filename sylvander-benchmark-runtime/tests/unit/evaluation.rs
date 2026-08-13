use super::*;
use crate::{
    BenchmarkModelBinding, RuntimeBenchCoordinate, ScenarioFamily, TopologyProfile,
    WorkspaceProfile,
};

fn result(ordinal: u32, profile: CognitionProfile, reward: f64) -> RuntimeBenchResult {
    let (models, auxiliary_model_calls, perception_calls) = match profile {
        CognitionProfile::PrimaryOnly => (
            vec![BenchmarkModelBinding {
                role: BenchmarkModelRole::Primary,
                model: "provider/primary".into(),
            }],
            0,
            0,
        ),
        CognitionProfile::PerceptionSpecialist => (
            vec![
                BenchmarkModelBinding {
                    role: BenchmarkModelRole::Primary,
                    model: "provider/primary".into(),
                },
                BenchmarkModelBinding {
                    role: BenchmarkModelRole::Audio,
                    model: "provider/audio".into(),
                },
            ],
            1,
            1,
        ),
        _ => unreachable!("test uses only perception pairs"),
    };
    RuntimeBenchResult {
        coordinate: RuntimeBenchCoordinate {
            suite: "paired-perception".into(),
            suite_revision: "v1".into(),
            scenario_id: "speech-under-noise".into(),
            family: ScenarioFamily::MultimodalPerception,
            topology: TopologyProfile::SingleAgent,
            workspace: WorkspaceProfile::ReadOnlyShared,
            failure_point: FailurePoint::None,
            cognition: profile,
            models,
            run_ordinal: ordinal,
        },
        verifier_reward: Some(reward),
        useful_completion: true,
        invariant_violations: 0,
        duplicate_effects: 0,
        user_visible_failures: 0,
        recovered: false,
        duration_millis: if profile == CognitionProfile::PrimaryOnly {
            1_000
        } else {
            1_100
        },
        input_tokens: if profile == CognitionProfile::PrimaryOnly {
            800
        } else {
            880
        },
        output_tokens: if profile == CognitionProfile::PrimaryOnly {
            200
        } else {
            220
        },
        model_calls: 1 + auxiliary_model_calls,
        primary_model_calls: 1,
        auxiliary_model_calls,
        perception_calls,
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
    }
}

fn policy() -> ActivationGatePolicy {
    ActivationGatePolicy {
        minimum_pairs: 3,
        minimum_reward_gain_micros: 50_000,
        minimum_quality_win_basis_points: 6_000,
        maximum_token_increase_basis_points: 2_000,
        maximum_p95_latency_increase_basis_points: 2_000,
    }
}

#[test]
fn matched_quality_gain_with_bounded_cost_is_eligible() {
    let baseline = (1..=5)
        .map(|ordinal| result(ordinal, CognitionProfile::PrimaryOnly, 0.60))
        .collect::<Vec<_>>();
    let candidate = (1..=5)
        .map(|ordinal| result(ordinal, CognitionProfile::PerceptionSpecialist, 0.70))
        .collect::<Vec<_>>();

    let report = evaluate_cognition_activation(
        &baseline,
        &candidate,
        CognitionProfile::PerceptionSpecialist,
        policy(),
    )
    .unwrap();
    assert_eq!(report.decision, ActivationDecision::Eligible);
    assert_eq!(report.pairs, 5);
    assert_eq!(report.median_reward_gain_micros, 100_000);
    assert_eq!(report.quality_win_basis_points, 10_000);
    assert_eq!(report.median_token_increase_basis_points, 1_000);
    assert_eq!(report.p95_latency_increase_basis_points, 1_000);
}

#[test]
fn one_safety_regression_keeps_the_candidate_disabled() {
    let baseline = (1..=3)
        .map(|ordinal| result(ordinal, CognitionProfile::PrimaryOnly, 0.60))
        .collect::<Vec<_>>();
    let mut candidate = (1..=3)
        .map(|ordinal| result(ordinal, CognitionProfile::PerceptionSpecialist, 0.80))
        .collect::<Vec<_>>();
    candidate[1].duplicate_effects = 1;

    let report = evaluate_cognition_activation(
        &baseline,
        &candidate,
        CognitionProfile::PerceptionSpecialist,
        policy(),
    )
    .unwrap();
    assert_eq!(report.decision, ActivationDecision::KeepDisabled);
    assert_eq!(report.unsafe_candidates, 1);
}

#[test]
fn missing_pair_or_changed_primary_model_is_rejected() {
    let baseline = vec![result(1, CognitionProfile::PrimaryOnly, 0.60)];
    let mut candidate = vec![result(2, CognitionProfile::PerceptionSpecialist, 0.80)];
    assert_eq!(
        evaluate_cognition_activation(
            &baseline,
            &candidate,
            CognitionProfile::PerceptionSpecialist,
            policy(),
        ),
        Err(ActivationGateError::UnpairedEvidence)
    );
    candidate[0].coordinate.run_ordinal = 1;
    candidate[0].coordinate.models[0].model = "provider/other-primary".into();
    assert_eq!(
        evaluate_cognition_activation(
            &baseline,
            &candidate,
            CognitionProfile::PerceptionSpecialist,
            policy(),
        ),
        Err(ActivationGateError::PrimaryModelMismatch)
    );
}
