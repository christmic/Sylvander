use super::*;
use crate::agent::instance::{
    AgentDefinitionKey, AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute,
    HistoryView, SessionAgentRole,
};
use crate::session::membership::{SessionGovernance, SessionMembership};
use sylvander_api::{AgentId, AgentInstanceId};

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
            participant("worker", SessionAgentRole::Worker),
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

fn task(id: &str, parent: Option<&str>) -> CoordinationTask {
    CoordinationTask {
        task_id: TaskId::new(id),
        session_id: SessionId::new("session"),
        membership_revision: 0,
        parent_task_id: parent.map(TaskId::new),
        created_by: AgentInstanceId::new("moderator"),
        assigned_to: Some(AgentInstanceId::new("worker")),
        objective: format!("complete {id}"),
        state: CoordinationTaskState::Ready,
        token_budget: 1_000,
        consumed_tokens: 0,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 1,
        updated_at: 1,
    }
}

#[test]
fn valid_task_dag_is_accepted() {
    let graph = SessionTaskGraph {
        session_id: SessionId::new("session"),
        membership_revision: 0,
        tasks: vec![task("root", None), task("child", Some("root"))],
        dependencies: vec![TaskDependency {
            prerequisite: TaskId::new("root"),
            dependent: TaskId::new("child"),
        }],
    };

    graph.validate(&membership()).unwrap();
}

#[test]
fn dependency_cycle_is_rejected_before_execution() {
    let graph = SessionTaskGraph {
        session_id: SessionId::new("session"),
        membership_revision: 0,
        tasks: vec![task("a", None), task("b", None)],
        dependencies: vec![
            TaskDependency {
                prerequisite: TaskId::new("a"),
                dependent: TaskId::new("b"),
            },
            TaskDependency {
                prerequisite: TaskId::new("b"),
                dependent: TaskId::new("a"),
            },
        ],
    };

    assert_eq!(
        graph.validate(&membership()).unwrap_err(),
        TaskGraphError::DependencyCycle
    );
}

#[test]
fn terminal_tasks_cannot_be_reopened() {
    assert!(!CoordinationTaskState::Completed.can_transition_to(CoordinationTaskState::Running));
    assert!(CoordinationTaskState::Blocked.can_transition_to(CoordinationTaskState::Ready));
}
