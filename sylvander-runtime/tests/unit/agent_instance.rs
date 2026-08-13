use super::*;

#[test]
fn terminal_instance_states_cannot_restart() {
    for state in [
        AgentInstanceState::Completed,
        AgentInstanceState::Failed,
        AgentInstanceState::Cancelled,
    ] {
        assert!(state.is_terminal());
        assert!(!state.can_transition_to(AgentInstanceState::Ready));
        assert!(!state.can_transition_to(AgentInstanceState::Running));
    }
}

#[test]
fn manual_reconciliation_requires_an_explicit_resolution() {
    assert!(AgentInstanceState::Ready.can_transition_to(AgentInstanceState::ManualReconciliation));
    assert!(
        AgentInstanceState::Running.can_transition_to(AgentInstanceState::ManualReconciliation)
    );
    assert!(AgentInstanceState::ManualReconciliation.can_transition_to(AgentInstanceState::Ready));
    assert!(
        !AgentInstanceState::ManualReconciliation.can_transition_to(AgentInstanceState::Running)
    );
}

#[test]
fn only_the_moderator_role_is_root_moderator() {
    assert!(SessionAgentRole::Moderator.is_root_moderator());
    assert!(!SessionAgentRole::Worker.is_root_moderator());
    assert!(!SessionAgentRole::Reviewer.is_root_moderator());
}
