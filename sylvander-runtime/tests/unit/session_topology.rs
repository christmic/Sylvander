use sylvander_api::{AgentId, AgentInstanceId, SessionId};

use super::{AgentRelation, AgentRelationKind, SessionTopology, TopologyError};
use crate::agent::instance::{
    AgentDefinitionKey, AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute,
    HistoryView, SessionAgentRole,
};
use crate::session::membership::{SessionGovernance, SessionMembership};

fn instance(id: &str, role: SessionAgentRole) -> AgentInstance {
    AgentInstance {
        instance_id: AgentInstanceId::new(id),
        session_id: SessionId::new("session"),
        definition: AgentDefinitionKey {
            agent_id: AgentId::new(id),
            revision: 1,
        },
        origin: AgentInstanceOrigin::Defined,
        role,
        history_view: HistoryView::SharedLane { cursor: 0 },
        approval_route: ApprovalRoute::Moderator {
            instance_id: AgentInstanceId::new("moderator"),
        },
        state: AgentInstanceState::Ready,
        lifecycle_revision: 0,
        capability_revision: "capability-v1".into(),
        created_at: 1,
        updated_at: 1,
    }
}

fn membership() -> SessionMembership {
    SessionMembership::new(
        SessionId::new("session"),
        vec![
            instance("moderator", SessionAgentRole::Moderator),
            instance("worker", SessionAgentRole::Worker),
            instance("reviewer", SessionAgentRole::Reviewer),
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
    .unwrap()
}

fn parent(source: &str, target: &str) -> AgentRelation {
    AgentRelation {
        source: AgentInstanceId::new(source),
        target: AgentInstanceId::new(target),
        kind: AgentRelationKind::ParentOf,
        created_at: 1,
    }
}

#[test]
fn moderator_rooted_hierarchy_accepts_peer_and_review_edges() {
    let mut relations = vec![
        parent("moderator", "worker"),
        parent("moderator", "reviewer"),
    ];
    relations.push(AgentRelation {
        source: AgentInstanceId::new("worker"),
        target: AgentInstanceId::new("reviewer"),
        kind: AgentRelationKind::Peer,
        created_at: 1,
    });
    relations.push(AgentRelation {
        source: AgentInstanceId::new("reviewer"),
        target: AgentInstanceId::new("worker"),
        kind: AgentRelationKind::Reviews,
        created_at: 1,
    });

    SessionTopology::new(SessionId::new("session"), 0, relations, &membership()).unwrap();
}

#[test]
fn disconnected_ownership_is_rejected() {
    let error = SessionTopology::new(
        SessionId::new("session"),
        0,
        vec![parent("moderator", "worker")],
        &membership(),
    )
    .unwrap_err();

    assert!(matches!(error, TopologyError::UnreachableFromModerator(_)));
}

#[test]
fn ancestor_cycle_is_rejected() {
    let error = SessionTopology::new(
        SessionId::new("session"),
        0,
        vec![parent("worker", "reviewer"), parent("reviewer", "worker")],
        &membership(),
    )
    .unwrap_err();

    assert!(matches!(error, TopologyError::OwnershipCycle(_)));
}

#[test]
fn symmetric_peer_duplicates_are_rejected() {
    let peer = |source, target| AgentRelation {
        source: AgentInstanceId::new(source),
        target: AgentInstanceId::new(target),
        kind: AgentRelationKind::Peer,
        created_at: 1,
    };
    let error = SessionTopology::new(
        SessionId::new("session"),
        0,
        vec![
            parent("moderator", "worker"),
            parent("moderator", "reviewer"),
            peer("worker", "reviewer"),
            peer("reviewer", "worker"),
        ],
        &membership(),
    )
    .unwrap_err();

    assert_eq!(error, TopologyError::DuplicateRelation);
}
