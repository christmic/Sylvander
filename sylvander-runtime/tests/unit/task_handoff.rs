use sylvander_api::{AgentId, AgentInstanceId, HandoffId, SessionId, SwarmId, TaskId};

use super::*;
use crate::agent::instance::{
    AgentDefinitionKey, AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute,
    HistoryView, SessionAgentRole,
};
use crate::coordination::task::{CoordinationTask, CoordinationTaskState};
use crate::coordination::topology::{AgentRelation, AgentRelationKind};
use crate::session::membership::{SessionGovernance, SessionMembership};

fn membership() -> SessionMembership {
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
    SessionMembership::new(
        SessionId::new("session"),
        vec![
            participant("moderator", SessionAgentRole::Moderator),
            participant(
                "left",
                SessionAgentRole::Coordinator {
                    swarm_id: SwarmId::new("left"),
                },
            ),
            participant(
                "right",
                SessionAgentRole::Coordinator {
                    swarm_id: SwarmId::new("right"),
                },
            ),
            participant("worker-a", SessionAgentRole::Worker),
            participant("worker-b", SessionAgentRole::Worker),
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

fn topology(membership: &SessionMembership) -> SessionTopology {
    let edge = |source, target| AgentRelation {
        source: AgentInstanceId::new(source),
        target: AgentInstanceId::new(target),
        kind: AgentRelationKind::ParentOf,
        created_at: 1,
    };
    SessionTopology::new(
        SessionId::new("session"),
        0,
        3,
        vec![
            edge("moderator", "left"),
            edge("moderator", "right"),
            edge("left", "worker-a"),
            edge("right", "worker-b"),
        ],
        1,
        membership,
    )
    .unwrap()
}

fn task() -> CoordinationTask {
    CoordinationTask {
        task_id: TaskId::new("task"),
        session_id: SessionId::new("session"),
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator"),
        assigned_to: Some(AgentInstanceId::new("worker-a")),
        objective: "cross-check the result".into(),
        state: CoordinationTaskState::Running,
        token_budget: 1_000,
        consumed_tokens: 100,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 4,
        created_at: 1,
        updated_at: 2,
    }
}

fn handoff(arbitrator: &str) -> TaskHandoff {
    TaskHandoff {
        handoff_id: HandoffId::new("handoff"),
        session_id: SessionId::new("session"),
        task_id: TaskId::new("task"),
        from_instance_id: AgentInstanceId::new("worker-a"),
        to_instance_id: AgentInstanceId::new("worker-b"),
        requested_by: AgentInstanceId::new("worker-a"),
        arbitrator_instance_id: AgentInstanceId::new(arbitrator),
        task_revision: 4,
        topology_revision: 3,
        reason: "worker-b owns the required evidence".into(),
        state: HandoffState::Proposed,
        revision: 0,
        expires_at: 100,
        created_at: 1,
        updated_at: 1,
    }
}

#[test]
fn cross_branch_handoff_routes_to_root_moderator() {
    let membership = membership();
    let topology = topology(&membership);

    handoff("moderator")
        .validate_proposal(&task(), &topology, &membership, 50)
        .unwrap();
}

#[test]
fn subordinate_cannot_self_appoint_the_wrong_arbitrator() {
    let membership = membership();
    let topology = topology(&membership);
    let error = handoff("left")
        .validate_proposal(&task(), &topology, &membership, 50)
        .unwrap_err();

    assert_eq!(
        error,
        HandoffError::WrongArbitrator(AgentInstanceId::new("moderator"))
    );
}
