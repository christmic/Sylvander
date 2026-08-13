use sylvander_agent::tool::invocation::ToolRecoveryPolicy;

use super::*;

#[test]
fn execution_positions_only_advance_one_boundary() {
    let positions = [
        ToolExecutionPosition::Prepared,
        ToolExecutionPosition::Authorized,
        ToolExecutionPosition::EffectStarted,
        ToolExecutionPosition::EffectCommitted,
        ToolExecutionPosition::ResultPersisted,
    ];
    for (current_index, current) in positions.iter().copied().enumerate() {
        for (next_index, next) in positions.iter().copied().enumerate() {
            assert_eq!(
                current.can_advance_to(next),
                next_index == current_index + 1,
                "unexpected transition {current:?} -> {next:?}",
            );
        }
    }
}

#[test]
fn effect_started_uses_policy_not_authority() {
    let cases = [
        (
            ToolRecoveryPolicy::NeverReplay,
            ToolRecoveryDecision::ManualReconciliation,
            true,
        ),
        (
            ToolRecoveryPolicy::RetryWithSameInvocation,
            ToolRecoveryDecision::RetrySameInvocation,
            false,
        ),
        (
            ToolRecoveryPolicy::ReconcileBeforeRetry,
            ToolRecoveryDecision::Reconcile,
            false,
        ),
    ];
    for (policy, expected, manual) in cases {
        let classification =
            RecoveryClassification::for_interrupted(ToolExecutionPosition::EffectStarted, policy);
        assert_eq!(classification.decision, expected);
        assert_eq!(classification.operator_action_required, manual);
    }
}

#[test]
fn committed_effect_is_never_selected_for_reexecution() {
    for policy in [
        ToolRecoveryPolicy::NeverReplay,
        ToolRecoveryPolicy::RetryWithSameInvocation,
        ToolRecoveryPolicy::ReconcileBeforeRetry,
    ] {
        assert_eq!(
            RecoveryClassification::for_interrupted(
                ToolExecutionPosition::EffectCommitted,
                policy,
            )
            .decision,
            ToolRecoveryDecision::ManualReconciliation,
        );
        assert!(
            RecoveryClassification::for_interrupted(
                ToolExecutionPosition::EffectCommitted,
                policy,
            )
            .operator_action_required
        );
        assert_eq!(
            RecoveryClassification::for_interrupted(
                ToolExecutionPosition::ResultPersisted,
                policy,
            )
            .decision,
            ToolRecoveryDecision::ContinueTurn,
        );
    }
}

#[test]
fn invocation_identity_is_valid_and_unique() {
    let first = ToolInvocationId::new();
    let second = ToolInvocationId::new();
    assert_ne!(first, second);
    assert_eq!(
        ToolInvocationId::parse(first.as_str().to_owned()).unwrap(),
        first,
    );
    assert!(ToolInvocationId::parse("provider-call-id".into()).is_err());
}
