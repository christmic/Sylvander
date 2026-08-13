use super::active_lease_decision;
use crate::storage::session::ToolRecoveryDecision;

#[test]
fn active_classification_lease_is_observed_without_reacquisition() {
    assert_eq!(
        active_lease_decision(
            Some(ToolRecoveryDecision::ManualReconciliation),
            Some(130),
            100,
        ),
        Some(ToolRecoveryDecision::ManualReconciliation)
    );
    assert_eq!(
        active_lease_decision(
            Some(ToolRecoveryDecision::RetrySameInvocation),
            Some(100),
            100,
        ),
        None
    );
    assert_eq!(
        active_lease_decision::<ToolRecoveryDecision>(None, Some(130), 100),
        None
    );
}
