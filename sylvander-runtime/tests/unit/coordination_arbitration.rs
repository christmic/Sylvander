use crate::agent::instance::{
    AgentDefinitionKey, AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute,
    HistoryView, SessionAgentRole,
};
use crate::coordination::governance::GovernanceFinding;
use crate::coordination::task::{CoordinationTask, CoordinationTaskState};
use crate::session::membership::SessionGovernance;
use sylvander_api::AgentId;

use super::*;

fn membership() -> SessionMembership {
    let session_id = SessionId::new("session");
    let participant = |id: &str, role| AgentInstance {
        instance_id: AgentInstanceId::new(id),
        session_id: session_id.clone(),
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
        capability_revision: "sha256:capabilities".into(),
        created_at: 1,
        updated_at: 1,
    };
    SessionMembership::new(
        session_id.clone(),
        vec![
            participant("moderator", SessionAgentRole::Moderator),
            participant("worker", SessionAgentRole::Worker),
        ],
        SessionGovernance {
            session_id,
            moderator_instance_id: AgentInstanceId::new("moderator"),
            governance_revision: "sha256:governance".into(),
            membership_revision: 3,
            lease_epoch: 7,
            fencing_token: 11,
            updated_at: 1,
        },
    )
    .unwrap()
}

fn task_graph() -> SessionTaskGraph {
    SessionTaskGraph {
        session_id: SessionId::new("session"),
        membership_revision: 3,
        tasks: vec![CoordinationTask {
            task_id: TaskId::new("task"),
            session_id: SessionId::new("session"),
            membership_revision: 3,
            parent_task_id: None,
            created_by: AgentInstanceId::new("moderator"),
            assigned_to: Some(AgentInstanceId::new("worker")),
            objective: "finish governed work".into(),
            state: CoordinationTaskState::Running,
            token_budget: 100,
            consumed_tokens: 20,
            max_handoffs: 2,
            handoff_count: 0,
            revision: 1,
            created_at: 1,
            updated_at: 2,
        }],
        dependencies: Vec::new(),
    }
}

fn arbitration_case(finding: GovernanceFinding) -> ArbitrationCase {
    ArbitrationCase {
        case_id: GovernanceCaseId::new("case"),
        session_id: SessionId::new("session"),
        moderator_instance_id: AgentInstanceId::new("moderator"),
        membership_revision: 3,
        topology_revision: 5,
        moderator_lease_epoch: 7,
        moderator_fencing_token: 11,
        findings: vec![finding],
        state: ArbitrationState::Open,
        revision: 0,
        expires_at: 100,
        created_at: 1,
        updated_at: 1,
    }
}

fn continue_decision(actor: &str) -> ModeratorDecision {
    ModeratorDecision {
        case_id: GovernanceCaseId::new("case"),
        decided_by: AgentInstanceId::new(actor),
        moderator_lease_epoch: 7,
        moderator_fencing_token: 11,
        verdict: ModeratorVerdict::ContinueWithConditions {
            conditions: vec!["produce a new evidence digest before another iteration".into()],
        },
        rationale: "one bounded retry can disambiguate transient stagnation".into(),
        evidence_refs: vec!["observation:3".into()],
        decided_at: 10,
    }
}

#[test]
fn only_the_fenced_session_moderator_can_decide() {
    let case = arbitration_case(GovernanceFinding::StagnantProgress {
        task_id: TaskId::new("task"),
        observations: 3,
    });

    assert_eq!(
        continue_decision("worker").validate(&case, &membership(), &task_graph(), 5, 10),
        Err(ArbitrationError::UnauthorizedModerator)
    );
}

#[test]
fn hard_stops_cannot_be_overridden_by_ai_continuation() {
    let case = arbitration_case(GovernanceFinding::WaitCycle {
        agents: vec![
            AgentInstanceId::new("moderator"),
            AgentInstanceId::new("worker"),
        ],
    });

    assert_eq!(
        continue_decision("moderator").validate(&case, &membership(), &task_graph(), 5, 10),
        Err(ArbitrationError::HardStopCannotContinue)
    );
}

#[test]
fn moderator_may_conditionally_continue_a_heuristic_case() {
    let case = arbitration_case(GovernanceFinding::StagnantProgress {
        task_id: TaskId::new("task"),
        observations: 3,
    });
    let membership = membership();
    case.validate_new(&membership, 5, 10).unwrap();

    continue_decision("moderator")
        .validate(&case, &membership, &task_graph(), 5, 10)
        .unwrap();
}
