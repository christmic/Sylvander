use super::*;

#[test]
fn controller_interrupts_exactly_the_selected_occurrence() {
    let mut controller = FaultController::new(FaultInjectionSpec {
        point: FailurePoint::ToolCommitted,
        occurrence: 2,
    })
    .unwrap();
    assert_eq!(
        controller.checkpoint(FailurePoint::ModelCommitted),
        FaultDecision::Continue
    );
    assert_eq!(
        controller.checkpoint(FailurePoint::ToolCommitted),
        FaultDecision::Continue
    );
    assert_eq!(
        controller.checkpoint(FailurePoint::ToolCommitted),
        FaultDecision::Interrupt(FaultReceipt {
            point: FailurePoint::ToolCommitted,
            occurrence: 2,
        })
    );
    assert_eq!(
        controller.checkpoint(FailurePoint::ToolCommitted),
        FaultDecision::Continue
    );
    assert!(controller.fired());
}

#[test]
fn controller_rejects_non_fault_coordinates() {
    let mut coordinate = RuntimeBenchCoordinate {
        suite: "runtime".into(),
        suite_revision: "v1".into(),
        scenario_id: "baseline".into(),
        family: ScenarioFamily::CrashRecovery,
        topology: TopologyProfile::SingleAgent,
        workspace: WorkspaceProfile::ReadOnlyShared,
        failure_point: FailurePoint::None,
        cognition: CognitionProfile::PrimaryOnly,
        models: vec!["provider/model".into()],
        run_ordinal: 1,
    };
    assert_eq!(
        FaultController::for_coordinate(&coordinate, 1).unwrap_err(),
        FaultInjectionError::InvalidSpec
    );
    coordinate.family = ScenarioFamily::MultiAgentCoordination;
    coordinate.failure_point = FailurePoint::MailboxDelivered;
    assert_eq!(
        FaultController::for_coordinate(&coordinate, 1).unwrap_err(),
        FaultInjectionError::InvalidCoordinate
    );
}
