use super::*;

fn binding(role: CognitiveRole, model: &str) -> CognitiveRoleBinding {
    CognitiveRoleBinding {
        role,
        model: ModelSelection {
            provider_id: "test".into(),
            model_id: model.into(),
        },
    }
}

#[test]
fn modality_and_safety_preempt_efficiency_within_budget() {
    let config = CognitionConfig {
        roles: vec![
            binding(CognitiveRole::Vision, "vision"),
            binding(CognitiveRole::Critic, "critic"),
            binding(CognitiveRole::Deliberation, "deep"),
            binding(CognitiveRole::FastDraft, "fast"),
        ],
        max_auxiliary_calls: 2,
    };
    let plan = config.plan(CognitiveSignals {
        modality: InputModality::Vision,
        complexity: 90,
        uncertainty: 80,
        risk: CognitiveRisk::High,
        prior_failures: 0,
    });
    assert_eq!(
        plan.auxiliary
            .iter()
            .map(|binding| binding.role)
            .collect::<Vec<_>>(),
        [CognitiveRole::Vision, CognitiveRole::Critic]
    );
    assert!(plan.primary_is_final_authority);
}

#[test]
fn simple_low_risk_text_can_use_fast_draft() {
    let config = CognitionConfig {
        roles: vec![binding(CognitiveRole::FastDraft, "fast")],
        max_auxiliary_calls: 1,
    };
    let plan = config.plan(CognitiveSignals {
        modality: InputModality::Text,
        complexity: 10,
        uncertainty: 5,
        risk: CognitiveRisk::Low,
        prior_failures: 0,
    });
    assert_eq!(plan.auxiliary[0].role, CognitiveRole::FastDraft);
}
