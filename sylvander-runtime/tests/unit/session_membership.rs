use crate::agent::instance::{
    AgentDefinitionKey, AgentInstanceOrigin, ApprovalRoute, HistoryView, SessionAgentRole,
};
use sylvander_api::{AgentId, AgentInstanceId};

use super::*;

fn participant(id: &str, role: SessionAgentRole) -> AgentInstance {
    AgentInstance {
        instance_id: AgentInstanceId::new(id),
        session_id: SessionId::new("session-1"),
        definition: AgentDefinitionKey {
            agent_id: AgentId::new("defined-agent"),
            revision: 1,
        },
        origin: AgentInstanceOrigin::Defined,
        role,
        history_view: HistoryView::SharedLane { cursor: 0 },
        approval_route: ApprovalRoute::User,
        state: AgentInstanceState::Ready,
        lifecycle_revision: 0,
        capability_revision: "sha256:capabilities".into(),
        created_at: 1,
        updated_at: 1,
    }
}

fn governance(moderator: &str) -> SessionGovernance {
    SessionGovernance {
        session_id: SessionId::new("session-1"),
        moderator_instance_id: AgentInstanceId::new(moderator),
        governance_revision: "sha256:governance".into(),
        lease_epoch: 1,
        fencing_token: 1,
        updated_at: 1,
    }
}

#[test]
fn session_accepts_multiple_first_class_agent_instances() {
    let membership = SessionMembership::new(
        SessionId::new("session-1"),
        vec![
            participant("moderator", SessionAgentRole::Moderator),
            participant("worker-1", SessionAgentRole::Worker),
            participant("worker-2", SessionAgentRole::Reviewer),
        ],
        governance("moderator"),
    )
    .unwrap();

    assert_eq!(membership.participants.len(), 3);
    assert_eq!(membership.moderator().instance_id.0, "moderator");
}

#[test]
fn session_rejects_multiple_root_moderators() {
    let result = SessionMembership::new(
        SessionId::new("session-1"),
        vec![
            participant("moderator-1", SessionAgentRole::Moderator),
            participant("moderator-2", SessionAgentRole::Moderator),
        ],
        governance("moderator-1"),
    );

    assert_eq!(
        result.unwrap_err(),
        SessionMembershipError::ModeratorCount(2)
    );
}

#[test]
fn session_rejects_duplicate_instance_identity() {
    let result = SessionMembership::new(
        SessionId::new("session-1"),
        vec![
            participant("same", SessionAgentRole::Moderator),
            participant("same", SessionAgentRole::Worker),
        ],
        governance("same"),
    );

    assert_eq!(
        result.unwrap_err(),
        SessionMembershipError::DuplicateParticipant(AgentInstanceId::new("same"))
    );
}

#[test]
fn terminal_moderator_cannot_govern_the_session() {
    let mut moderator = participant("moderator", SessionAgentRole::Moderator);
    moderator.state = AgentInstanceState::Failed;
    let result = SessionMembership::new(
        SessionId::new("session-1"),
        vec![moderator],
        governance("moderator"),
    );

    assert_eq!(
        result.unwrap_err(),
        SessionMembershipError::ModeratorUnavailable
    );
}
