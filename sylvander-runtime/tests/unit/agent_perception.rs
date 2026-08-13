use super::*;
use crate::agent::cognition::{CognitionConfig, CognitiveRoleBinding};
use sylvander_api::ModelSelection;

fn specialist(role: CognitiveRole) -> CognitionConfig {
    CognitionConfig {
        roles: vec![CognitiveRoleBinding {
            role,
            model: ModelSelection {
                provider_id: "test".into(),
                model_id: "specialist".into(),
            },
        }],
        max_auxiliary_calls: 1,
    }
}

#[test]
fn native_primary_is_preferred_over_a_specialist() {
    assert_eq!(
        plan_perception(
            &specialist(CognitiveRole::Vision),
            PerceptionSignals {
                modality: PerceptionModality::Image,
                transport_supported: true,
                primary_capabilities: ModelCapabilities::VISION,
            },
        ),
        PerceptionPlan::NativePrimary
    );
}

#[test]
fn missing_primary_capability_selects_only_the_matching_internal_role() {
    assert!(matches!(
        plan_perception(
            &specialist(CognitiveRole::Document),
            PerceptionSignals {
                modality: PerceptionModality::Document,
                transport_supported: true,
                primary_capabilities: ModelCapabilities::empty(),
            },
        ),
        PerceptionPlan::SpecialistCandidate { binding }
            if binding.role == CognitiveRole::Document
    ));
}

#[test]
fn unsupported_transport_cannot_be_bypassed_by_a_configured_role() {
    assert_eq!(
        plan_perception(
            &specialist(CognitiveRole::Audio),
            PerceptionSignals {
                modality: PerceptionModality::Audio,
                transport_supported: false,
                primary_capabilities: ModelCapabilities::empty(),
            },
        ),
        PerceptionPlan::Unavailable {
            reason: PerceptionUnavailableReason::TransportUnsupported,
        }
    );
}
