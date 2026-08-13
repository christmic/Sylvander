use super::*;

#[test]
fn positions_advance_only_across_one_durable_boundary() {
    assert!(
        PerceptionExecutionPosition::Prepared
            .can_advance_to(PerceptionExecutionPosition::MediaPersisted)
    );
    assert!(
        !PerceptionExecutionPosition::Prepared
            .can_advance_to(PerceptionExecutionPosition::InferenceStarted)
    );
    assert!(
        !PerceptionExecutionPosition::ArtifactPersisted
            .can_advance_to(PerceptionExecutionPosition::InferenceCompleted)
    );
}

#[test]
fn uncertain_inference_uses_its_independent_recovery_contract() {
    let never = PerceptionRecoveryClassification::for_interrupted(
        PerceptionExecutionPosition::InferenceStarted,
        PerceptionRecoveryPolicy::NeverReplay,
    );
    assert_eq!(
        never.decision,
        PerceptionRecoveryDecision::ManualReconciliation
    );
    assert!(never.operator_action_required);
    assert_eq!(
        PerceptionRecoveryClassification::for_interrupted(
            PerceptionExecutionPosition::InferenceStarted,
            PerceptionRecoveryPolicy::RetryWithSameInvocation,
        )
        .decision,
        PerceptionRecoveryDecision::RetrySameInvocation
    );
    assert_eq!(
        PerceptionRecoveryClassification::for_interrupted(
            PerceptionExecutionPosition::InferenceStarted,
            PerceptionRecoveryPolicy::RecoverFromReceipt,
        )
        .decision,
        PerceptionRecoveryDecision::RecoverReceipt
    );
}

#[test]
fn invocation_identity_is_stable_and_validated() {
    let id = PerceptionInvocationId::new();
    assert_eq!(PerceptionInvocationId::parse(id.as_str()).unwrap(), id);
    assert!(PerceptionInvocationId::parse("not-a-uuid").is_err());
}
