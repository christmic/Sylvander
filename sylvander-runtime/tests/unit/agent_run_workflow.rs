use super::*;

#[test]
fn every_runtime_state_has_an_agent_visible_projection() {
    assert_eq!(
        agent_state(CoordinationTaskState::Proposed),
        WorkflowTaskState::Ready
    );
    assert_eq!(
        runtime_state(WorkflowTaskState::AwaitingReview),
        CoordinationTaskState::AwaitingReview
    );
    assert_eq!(
        runtime_state(WorkflowTaskState::Cancelled),
        CoordinationTaskState::Cancelled
    );
}
