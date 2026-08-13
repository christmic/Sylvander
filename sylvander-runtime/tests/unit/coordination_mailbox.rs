use sylvander_api::{AgentId, AgentInstanceId, CoordinationMessageId, SessionId};

use super::*;
use crate::agent::instance::{
    AgentDefinitionKey, AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute,
    HistoryView, SessionAgentRole,
};
use crate::coordination::topology::{AgentRelation, AgentRelationKind};
use crate::session::membership::{SessionGovernance, SessionMembership};

fn facts() -> (SessionMembership, SessionTopology) {
    let participant = |id, role| AgentInstance {
        instance_id: AgentInstanceId::new(id),
        session_id: SessionId::new("session"),
        definition: AgentDefinitionKey {
            agent_id: AgentId::new(id),
            revision: 1,
        },
        origin: AgentInstanceOrigin::Defined,
        role,
        history_view: HistoryView::SharedLane { cursor: 0 },
        approval_route: ApprovalRoute::User,
        state: AgentInstanceState::Ready,
        lifecycle_revision: 0,
        capability_revision: "capability-v1".into(),
        created_at: 1,
        updated_at: 1,
    };
    let membership = SessionMembership::new(
        SessionId::new("session"),
        vec![
            participant("moderator", SessionAgentRole::Moderator),
            participant("left", SessionAgentRole::Worker),
            participant("right", SessionAgentRole::Reviewer),
        ],
        SessionGovernance {
            session_id: SessionId::new("session"),
            moderator_instance_id: AgentInstanceId::new("moderator"),
            governance_revision: "governance-v1".into(),
            membership_revision: 0,
            lease_epoch: 1,
            fencing_token: 1,
            updated_at: 1,
        },
    )
    .unwrap();
    let edge = |target| AgentRelation {
        source: AgentInstanceId::new("moderator"),
        target: AgentInstanceId::new(target),
        kind: AgentRelationKind::ParentOf,
        created_at: 1,
    };
    let topology = SessionTopology::new(
        SessionId::new("session"),
        0,
        2,
        vec![edge("left"), edge("right")],
        1,
        &membership,
    )
    .unwrap();
    (membership, topology)
}

fn message(route: Vec<AgentInstanceId>) -> CoordinationMessage {
    CoordinationMessage {
        message_id: CoordinationMessageId::new("message"),
        session_id: SessionId::new("session"),
        sender_instance_id: AgentInstanceId::new("left"),
        recipient_instance_id: AgentInstanceId::new("right"),
        task_id: None,
        kind: CoordinationMessageKind::Evidence,
        payload: "result digest sha256:abc".into(),
        topology_revision: 2,
        route,
        max_hops: 4,
        state: MessageDeliveryState::Pending,
        delivery_attempts: 0,
        revision: 0,
        expires_at: 100,
        created_at: 1,
        updated_at: 1,
    }
}

#[test]
fn mailbox_accepts_governed_shortest_route() {
    let (membership, topology) = facts();
    let route = topology
        .route_between(
            &AgentInstanceId::new("left"),
            &AgentInstanceId::new("right"),
        )
        .unwrap();

    message(route)
        .validate_new(&topology, &membership, 50)
        .unwrap();
}

#[test]
fn mailbox_rejects_route_cycles_before_enqueue() {
    let (membership, topology) = facts();
    let error = message(vec![
        AgentInstanceId::new("left"),
        AgentInstanceId::new("moderator"),
        AgentInstanceId::new("left"),
        AgentInstanceId::new("right"),
    ])
    .validate_new(&topology, &membership, 50)
    .unwrap_err();

    assert_eq!(error, MailboxError::RouteCycle);
}
