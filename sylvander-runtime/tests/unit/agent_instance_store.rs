use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::instance::{
    AgentDefinitionKey, AgentInstanceOrigin, ApprovalRoute, HistoryView, SessionAgentRole,
};
use crate::coordination::arbitration::{
    ArbitrationCase, ArbitrationState, ModeratorDecision, ModeratorVerdict,
};
use crate::coordination::governance::{
    GovernanceFinding, GovernancePolicy, ProgressObservation, WaitDependency,
};
use crate::coordination::handoff::{HandoffState, TaskHandoff};
use crate::coordination::mailbox::{
    CoordinationMessage, CoordinationMessageKind, MessageDeliveryState,
};
use crate::coordination::service::{
    CoordinationService, DispatchMessageOutcome, DispatchMessageRequest, ProposeHandoffRequest,
    ReportWaitRequest,
};
use crate::coordination::task::{CoordinationTask, CoordinationTaskState, TaskDependency};
use crate::coordination::topology::{AgentRelation, AgentRelationKind, SessionTopology};
use crate::session::SessionMetadata;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::{SessionLifetime, SessionStore, StoredSession};
use sylvander_api::{
    AgentId, AgentInstanceId, CoordinationMessageId, GovernanceCaseId, HandoffId, SwarmId, TaskId,
};

use super::*;

fn stored_session() -> StoredSession {
    StoredSession::new(
        SessionId::new("multi-session"),
        "multi-session",
        SessionLifetime::Persistent,
        SessionMetadata {
            workspace: PathBuf::from("/tmp/project"),
            name: "multi-session".into(),
            user_id: "user-1".into(),
        },
        vec![AgentId::new("orchestrator")],
    )
}

fn instance(id: &str, agent: &str, role: SessionAgentRole) -> AgentInstance {
    AgentInstance {
        instance_id: AgentInstanceId::new(id),
        session_id: SessionId::new("multi-session"),
        definition: AgentDefinitionKey {
            agent_id: AgentId::new(agent),
            revision: 7,
        },
        origin: AgentInstanceOrigin::Defined,
        role,
        history_view: HistoryView::SharedLane { cursor: 0 },
        approval_route: ApprovalRoute::User,
        state: AgentInstanceState::Ready,
        lifecycle_revision: 0,
        capability_revision: format!("sha256:{id}"),
        created_at: 10,
        updated_at: 10,
    }
}

fn membership() -> SessionMembership {
    SessionMembership::new(
        SessionId::new("multi-session"),
        vec![
            instance("moderator-1", "orchestrator", SessionAgentRole::Moderator),
            instance("worker-1", "researcher", SessionAgentRole::Worker),
            instance(
                "coordinator-1",
                "orchestrator",
                SessionAgentRole::Coordinator {
                    swarm_id: SwarmId::new("swarm-1"),
                },
            ),
        ],
        SessionGovernance {
            session_id: SessionId::new("multi-session"),
            moderator_instance_id: AgentInstanceId::new("moderator-1"),
            governance_revision: "sha256:governance".into(),
            membership_revision: 0,
            lease_epoch: 3,
            fencing_token: 9,
            updated_at: 10,
        },
    )
    .unwrap()
}

fn topology(membership: &SessionMembership) -> SessionTopology {
    SessionTopology::new(
        SessionId::new("multi-session"),
        0,
        0,
        vec![
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("worker-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("coordinator-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
        ],
        11,
        membership,
    )
    .unwrap()
}

fn dispatch_request(id: &str) -> DispatchMessageRequest {
    DispatchMessageRequest {
        message_id: CoordinationMessageId::new(id),
        session_id: SessionId::new("multi-session"),
        sender_instance_id: AgentInstanceId::new("worker-1"),
        recipient_instance_id: AgentInstanceId::new("coordinator-1"),
        task_id: None,
        kind: CoordinationMessageKind::Evidence,
        payload: "sha256:evidence".into(),
        max_hops: 4,
        expires_at: 100,
    }
}

#[tokio::test]
async fn coordination_service_derives_route_from_durable_topology() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);

    let DispatchMessageOutcome::Enqueued(message) = service
        .dispatch_message(dispatch_request("governed-message"), 20)
        .await
        .unwrap()
    else {
        panic!("healthy dispatch must not require arbitration");
    };

    assert_eq!(
        message.route,
        ["worker-1", "moderator-1", "coordinator-1"].map(AgentInstanceId::new)
    );
    assert_eq!(
        store.message(&message.message_id).await.unwrap(),
        Some(message.clone())
    );
    let repeated = service
        .dispatch_message(dispatch_request("governed-message"), 21)
        .await
        .unwrap();
    assert_eq!(repeated, DispatchMessageOutcome::Enqueued(message));
    let claim = service
        .claim_next_message(&AgentInstanceId::new("coordinator-1"), 22, 10)
        .await
        .unwrap()
        .unwrap();
    let delivered = service.mark_message_delivered(&claim, 23).await.unwrap();
    let acknowledged = service
        .acknowledge_message(&delivered, &AgentInstanceId::new("coordinator-1"), 24)
        .await
        .unwrap();
    assert_eq!(acknowledged.state, MessageDeliveryState::Acknowledged);
}

#[tokio::test]
async fn coordination_service_persists_moderator_case_before_blocking_dispatch() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let policy = GovernancePolicy {
        max_agents: 2,
        ..GovernancePolicy::default()
    };
    let service = CoordinationService::new(store.clone(), policy, 30);

    let DispatchMessageOutcome::RequiresArbitration { case, assessment } = service
        .dispatch_message(dispatch_request("blocked-message"), 20)
        .await
        .unwrap()
    else {
        panic!("hard stop must require moderator arbitration");
    };

    assert!(assessment.has_hard_stop());
    assert!(case.case_id.0.starts_with("message:"));
    assert_eq!(case.case_id.0.len(), 72);
    assert_eq!(
        store.arbitration_case(&case.case_id).await.unwrap(),
        Some(case.clone())
    );
    assert!(
        store
            .message(&CoordinationMessageId::new("blocked-message"))
            .await
            .unwrap()
            .is_none()
    );
    let notification = store
        .message(&CoordinationMessageId::new(format!(
            "arbitration:{}",
            case.case_id.0
        )))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notification.recipient_instance_id.0, "moderator-1");
    assert_eq!(notification.kind, CoordinationMessageKind::Control);
    assert!(matches!(
        service
            .dispatch_message(dispatch_request("blocked-message"), 22)
            .await
            .unwrap(),
        DispatchMessageOutcome::RequiresArbitration { .. }
    ));
}

#[tokio::test]
async fn coordination_service_recovers_handoff_at_arbitration_boundary() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    store
        .create_task(&CoordinationTask {
            task_id: TaskId::new("handoff-task"),
            session_id: SessionId::new("multi-session"),
            membership_revision: 0,
            parent_task_id: None,
            created_by: AgentInstanceId::new("moderator-1"),
            assigned_to: Some(AgentInstanceId::new("worker-1")),
            objective: "produce evidence".into(),
            state: CoordinationTaskState::Running,
            token_budget: 1_000,
            consumed_tokens: 100,
            max_handoffs: 2,
            handoff_count: 0,
            revision: 0,
            created_at: 10,
            updated_at: 10,
        })
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    let request = ProposeHandoffRequest {
        handoff_id: HandoffId::new("governed-handoff"),
        session_id: SessionId::new("multi-session"),
        task_id: TaskId::new("handoff-task"),
        from_instance_id: AgentInstanceId::new("worker-1"),
        to_instance_id: AgentInstanceId::new("coordinator-1"),
        requested_by: AgentInstanceId::new("worker-1"),
        reason: "specialist context required".into(),
        expires_at: 100,
    };

    let handoff = service.propose_handoff(request.clone(), 20).await.unwrap();
    assert_eq!(handoff.state, HandoffState::AwaitingArbitration);
    assert_eq!(handoff.arbitrator_instance_id.0, "moderator-1");
    assert_eq!(service.propose_handoff(request, 21).await.unwrap(), handoff);
    let accepted = service
        .decide_handoff(
            &SessionId::new("multi-session"),
            &handoff.handoff_id,
            &AgentInstanceId::new("moderator-1"),
            true,
            22,
        )
        .await
        .unwrap();
    assert_eq!(accepted.state, HandoffState::Accepted);
    assert_eq!(
        service
            .decide_handoff(
                &SessionId::new("multi-session"),
                &handoff.handoff_id,
                &AgentInstanceId::new("moderator-1"),
                true,
                23,
            )
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(
        store
            .task(&TaskId::new("handoff-task"))
            .await
            .unwrap()
            .unwrap()
            .assigned_to,
        Some(AgentInstanceId::new("coordinator-1"))
    );
}

#[tokio::test]
async fn coordination_service_dead_letters_poison_message_after_bounded_retries() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let policy = GovernancePolicy {
        max_message_delivery_attempts: 1,
        ..GovernancePolicy::default()
    };
    let service = CoordinationService::new(store.clone(), policy, 30);
    service
        .dispatch_message(dispatch_request("poison-message"), 20)
        .await
        .unwrap();

    let first = service
        .claim_next_message(&AgentInstanceId::new("coordinator-1"), 21, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.message.delivery_attempts, 1);
    assert!(
        service
            .claim_next_message(&AgentInstanceId::new("coordinator-1"), 22, 1)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .message(&CoordinationMessageId::new("poison-message"))
            .await
            .unwrap()
            .unwrap()
            .state,
        MessageDeliveryState::DeadLetter
    );
}

#[tokio::test]
async fn governance_wait_and_progress_facts_are_revision_fenced_and_idempotent() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let task = CoordinationTask {
        task_id: TaskId::new("observed-task"),
        session_id: SessionId::new("multi-session"),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "observe governed progress".into(),
        state: CoordinationTaskState::Running,
        token_budget: 1_000,
        consumed_tokens: 100,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 10,
        updated_at: 10,
    };
    store.create_task(&task).await.unwrap();
    let wait = WaitDependency {
        task_id: task.task_id.clone(),
        waiter: AgentInstanceId::new("worker-1"),
        awaited: AgentInstanceId::new("coordinator-1"),
    };
    store
        .record_wait(&task.session_id, &wait, 0, 0, 20)
        .await
        .unwrap();
    store
        .record_wait(&task.session_id, &wait, 0, 0, 21)
        .await
        .unwrap();
    assert!(
        store
            .record_wait(&task.session_id, &wait, 0, 1, 22)
            .await
            .is_err()
    );

    let progress = ProgressObservation {
        observation_id: "progress-1".into(),
        task_id: task.task_id.clone(),
        agent_instance_id: AgentInstanceId::new("worker-1"),
        task_revision: 0,
        consumed_tokens: 150,
        evidence_digest: Some("sha256:new-evidence".into()),
        observed_at: 20,
    };
    store
        .record_progress(&task.session_id, &progress)
        .await
        .unwrap();
    store
        .record_progress(&task.session_id, &progress)
        .await
        .unwrap();
    let mut conflicting = progress;
    conflicting.consumed_tokens = 151;
    assert!(
        store
            .record_progress(&task.session_id, &conflicting)
            .await
            .is_err()
    );
    let observations = store
        .governance_observations(&task.session_id, 4)
        .await
        .unwrap();
    assert_eq!(observations.waits.len(), 1);
    assert_eq!(observations.waits[0], wait);
    assert_eq!(observations.progress.len(), 1);
    assert_eq!(observations.progress[0].observation_id, "progress-1");
    assert!(observations.handoffs.is_empty());
    store.clear_wait(&task.session_id, &wait).await.unwrap();
    store.clear_wait(&task.session_id, &wait).await.unwrap();
    assert!(
        store
            .governance_observations(&task.session_id, 4)
            .await
            .unwrap()
            .waits
            .is_empty()
    );
}

#[tokio::test]
async fn durable_wait_cycle_blocks_dispatch_and_escalates_to_moderator() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let mut task = CoordinationTask {
        task_id: TaskId::new("cycle-left"),
        session_id: SessionId::new("multi-session"),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "left side".into(),
        state: CoordinationTaskState::Running,
        token_budget: 1_000,
        consumed_tokens: 10,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 10,
        updated_at: 10,
    };
    store.create_task(&task).await.unwrap();
    task.task_id = TaskId::new("cycle-right");
    task.assigned_to = Some(AgentInstanceId::new("coordinator-1"));
    task.objective = "right side".into();
    store.create_task(&task).await.unwrap();
    let service = CoordinationService::new(store, GovernancePolicy::default(), 30);
    service
        .report_wait(
            &ReportWaitRequest {
                session_id: SessionId::new("multi-session"),
                task_id: TaskId::new("cycle-left"),
                waiter: AgentInstanceId::new("worker-1"),
                awaited: AgentInstanceId::new("coordinator-1"),
            },
            20,
        )
        .await
        .unwrap();
    service
        .report_wait(
            &ReportWaitRequest {
                session_id: SessionId::new("multi-session"),
                task_id: TaskId::new("cycle-right"),
                waiter: AgentInstanceId::new("coordinator-1"),
                awaited: AgentInstanceId::new("worker-1"),
            },
            20,
        )
        .await
        .unwrap();

    let DispatchMessageOutcome::RequiresArbitration { assessment, .. } = service
        .dispatch_message(dispatch_request("cycle-blocked-message"), 21)
        .await
        .unwrap()
    else {
        panic!("durable wait cycle must block automatic dispatch");
    };
    assert!(assessment.findings.iter().any(|finding| matches!(
        finding,
        GovernanceFinding::WaitCycle { agents } if agents.len() == 2
    )));
}

#[tokio::test]
async fn multi_agent_membership_survives_file_restart() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("sessions.db");
    {
        let store = SqliteSessionStore::open(&path).await.unwrap();
        store.save(&stored_session()).await.unwrap();
        store
            .save_session_membership(&membership(), None)
            .await
            .unwrap();
    }

    let reopened = SqliteSessionStore::open(path).await.unwrap();
    let restored = reopened
        .session_membership(&SessionId::new("multi-session"))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(restored, membership());
    assert_eq!(restored.participants.len(), 3);
    assert_eq!(restored.moderator().instance_id.0, "moderator-1");
}

#[tokio::test]
async fn membership_requires_an_existing_active_session() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let error = store
        .save_session_membership(&membership(), None)
        .await
        .unwrap_err();

    assert!(matches!(error, SessionStoreError::NotFound(_)));
}

#[tokio::test]
async fn replacing_membership_removes_departed_instances_atomically() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    store
        .save_session_membership(&membership(), None)
        .await
        .unwrap();
    let reduced = SessionMembership::new(
        SessionId::new("multi-session"),
        vec![instance(
            "moderator-2",
            "orchestrator",
            SessionAgentRole::Moderator,
        )],
        SessionGovernance {
            moderator_instance_id: AgentInstanceId::new("moderator-2"),
            membership_revision: 1,
            fencing_token: 10,
            ..membership().governance
        },
    )
    .unwrap();

    store
        .save_session_membership(&reduced, Some(0))
        .await
        .unwrap();
    let restored = store
        .session_membership(&SessionId::new("multi-session"))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(restored, reduced);
    assert_eq!(restored.participants.len(), 1);
}

#[tokio::test]
async fn stale_membership_writer_is_rejected_without_overwriting_snapshot() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    store
        .save_session_membership(&membership(), None)
        .await
        .unwrap();

    let error = store
        .save_session_membership(&membership(), None)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        SessionStoreError::MembershipConflict {
            expected: None,
            actual: Some(0)
        }
    ));
    assert_eq!(
        store
            .session_membership(&SessionId::new("multi-session"))
            .await
            .unwrap()
            .unwrap(),
        membership()
    );
}

#[tokio::test]
async fn topology_is_durable_and_fenced_by_its_own_revision() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    let topology = SessionTopology::new(
        SessionId::new("multi-session"),
        0,
        0,
        vec![
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("worker-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("coordinator-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
        ],
        11,
        &membership,
    )
    .unwrap();

    store
        .save_topology(&topology, &membership, None)
        .await
        .unwrap();
    assert_eq!(
        store
            .topology(&SessionId::new("multi-session"))
            .await
            .unwrap(),
        Some(topology.clone())
    );
    assert!(matches!(
        store
            .save_topology(&topology, &membership, None)
            .await
            .unwrap_err(),
        SessionStoreError::TopologyConflict {
            expected: None,
            actual: Some(0)
        }
    ));
}

#[tokio::test]
async fn task_creation_is_durable_and_duplicate_safe() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    store
        .save_session_membership(&membership(), None)
        .await
        .unwrap();
    let task = CoordinationTask {
        task_id: TaskId::new("task-1"),
        session_id: SessionId::new("multi-session"),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "inspect recovery invariants".into(),
        state: CoordinationTaskState::Proposed,
        token_budget: 2_000,
        consumed_tokens: 0,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 12,
        updated_at: 12,
    };

    store.create_task(&task).await.unwrap();
    assert_eq!(store.task(&task.task_id).await.unwrap(), Some(task.clone()));
    assert!(matches!(
        store.create_task(&task).await.unwrap_err(),
        SessionStoreError::TaskConflict {
            expected: None,
            actual: Some(0),
            ..
        }
    ));

    let mut ready = task.clone();
    ready.state = CoordinationTaskState::Ready;
    ready.consumed_tokens = 25;
    ready.revision = 1;
    ready.updated_at = 13;
    store.update_task(&ready, 0).await.unwrap();
    assert_eq!(
        store.task(&task.task_id).await.unwrap(),
        Some(ready.clone())
    );

    let mut stale = ready.clone();
    stale.revision = 1;
    assert!(matches!(
        store.update_task(&stale, 0).await.unwrap_err(),
        SessionStoreError::TaskConflict {
            expected: Some(0),
            actual: Some(1),
            ..
        }
    ));

    let mut illicit_assignment = ready;
    illicit_assignment.assigned_to = Some(AgentInstanceId::new("coordinator-1"));
    illicit_assignment.state = CoordinationTaskState::Running;
    illicit_assignment.revision = 2;
    illicit_assignment.updated_at = 14;
    assert!(matches!(
        store.update_task(&illicit_assignment, 1).await.unwrap_err(),
        SessionStoreError::Invalid(_)
    ));
}

#[tokio::test]
async fn task_dependencies_are_durable_and_cycles_roll_back_atomically() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    let first = CoordinationTask {
        task_id: TaskId::new("task-first"),
        session_id: membership.session_id.clone(),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "collect evidence".into(),
        state: CoordinationTaskState::Ready,
        token_budget: 1_000,
        consumed_tokens: 0,
        max_handoffs: 1,
        handoff_count: 0,
        revision: 0,
        created_at: 12,
        updated_at: 12,
    };
    let mut second = first.clone();
    second.task_id = TaskId::new("task-second");
    second.assigned_to = Some(AgentInstanceId::new("coordinator-1"));
    second.objective = "synthesize evidence".into();
    store.create_task(&first).await.unwrap();
    store.create_task(&second).await.unwrap();

    let forward = TaskDependency {
        prerequisite: first.task_id.clone(),
        dependent: second.task_id.clone(),
    };
    store
        .add_task_dependency(&forward, &membership, 13)
        .await
        .unwrap();
    let graph = store
        .task_graph(&membership.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(graph.dependencies, std::slice::from_ref(&forward));

    let reverse = TaskDependency {
        prerequisite: second.task_id,
        dependent: first.task_id,
    };
    assert!(matches!(
        store
            .add_task_dependency(&reverse, &membership, 14)
            .await
            .unwrap_err(),
        SessionStoreError::Invalid(reason) if reason.contains("cycle")
    ));
    assert_eq!(
        store
            .task_graph(&membership.session_id)
            .await
            .unwrap()
            .unwrap()
            .dependencies,
        [forward]
    );
}

#[tokio::test]
async fn arbitration_case_is_durable_and_fenced_to_exact_governance_facts() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    let topology = SessionTopology::new(
        membership.session_id.clone(),
        0,
        0,
        vec![
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("worker-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("coordinator-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
        ],
        11,
        &membership,
    )
    .unwrap();
    store
        .save_topology(&topology, &membership, None)
        .await
        .unwrap();
    let task = CoordinationTask {
        task_id: TaskId::new("task-1"),
        session_id: membership.session_id.clone(),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "produce fresh evidence".into(),
        state: CoordinationTaskState::Running,
        token_budget: 1_000,
        consumed_tokens: 200,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 11,
        updated_at: 11,
    };
    store.create_task(&task).await.unwrap();
    let case = ArbitrationCase {
        case_id: GovernanceCaseId::new("case-1"),
        session_id: membership.session_id.clone(),
        moderator_instance_id: AgentInstanceId::new("moderator-1"),
        membership_revision: 0,
        topology_revision: 0,
        moderator_lease_epoch: 3,
        moderator_fencing_token: 9,
        findings: vec![GovernanceFinding::StagnantProgress {
            task_id: TaskId::new("task-1"),
            observations: 3,
        }],
        state: ArbitrationState::Open,
        revision: 0,
        expires_at: 100,
        created_at: 12,
        updated_at: 12,
    };

    store
        .create_arbitration_case(&case, &membership, &topology, 50)
        .await
        .unwrap();
    assert_eq!(
        store.arbitration_case(&case.case_id).await.unwrap(),
        Some(case.clone())
    );
    assert!(matches!(
        store
            .create_arbitration_case(&case, &membership, &topology, 50)
            .await
            .unwrap_err(),
        SessionStoreError::Invalid(reason) if reason.contains("already exists")
    ));

    let decision = ModeratorDecision {
        case_id: case.case_id.clone(),
        decided_by: AgentInstanceId::new("moderator-1"),
        moderator_lease_epoch: 3,
        moderator_fencing_token: 9,
        verdict: ModeratorVerdict::Replan {
            task_ids: vec![task.task_id],
        },
        rationale: "the current plan consumed tokens without new evidence".into(),
        evidence_refs: vec!["progress:3".into()],
        decided_at: 60,
    };
    let graph = store
        .task_graph(&membership.session_id)
        .await
        .unwrap()
        .unwrap();
    let decided = store
        .decide_arbitration(&decision, &membership, &graph, 0, 60)
        .await
        .unwrap();
    assert_eq!(decided.state, ArbitrationState::Decided);
    assert_eq!(decided.revision, 1);
    assert_eq!(
        store.arbitration_decision(&case.case_id).await.unwrap(),
        Some(decision.clone())
    );
    assert!(matches!(
        store
            .decide_arbitration(&decision, &membership, &graph, 0, 61)
            .await
            .unwrap_err(),
        SessionStoreError::Invalid(_)
    ));

    let mut stale = case;
    stale.case_id = GovernanceCaseId::new("case-stale");
    stale.moderator_fencing_token = 8;
    assert!(matches!(
        store
            .create_arbitration_case(&stale, &membership, &topology, 50)
            .await
            .unwrap_err(),
        SessionStoreError::Invalid(reason) if reason.contains("governance")
    ));
}

#[tokio::test]
async fn handoff_proposal_is_validated_persisted_and_deduplicated() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    let topology = SessionTopology::new(
        SessionId::new("multi-session"),
        0,
        0,
        vec![
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("worker-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("coordinator-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
        ],
        11,
        &membership,
    )
    .unwrap();
    store
        .save_topology(&topology, &membership, None)
        .await
        .unwrap();
    let task = CoordinationTask {
        task_id: TaskId::new("task-handoff"),
        session_id: SessionId::new("multi-session"),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "transfer governed work".into(),
        state: CoordinationTaskState::Running,
        token_budget: 2_000,
        consumed_tokens: 100,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 12,
        updated_at: 12,
    };
    store.create_task(&task).await.unwrap();
    let handoff = TaskHandoff {
        handoff_id: HandoffId::new("handoff-1"),
        session_id: task.session_id.clone(),
        task_id: task.task_id.clone(),
        from_instance_id: AgentInstanceId::new("worker-1"),
        to_instance_id: AgentInstanceId::new("coordinator-1"),
        requested_by: AgentInstanceId::new("worker-1"),
        arbitrator_instance_id: AgentInstanceId::new("moderator-1"),
        task_revision: 0,
        topology_revision: 0,
        reason: "coordinator owns the next stage".into(),
        state: HandoffState::Proposed,
        revision: 0,
        expires_at: 100,
        created_at: 13,
        updated_at: 13,
    };

    store
        .create_handoff(&handoff, &membership, &topology, 50)
        .await
        .unwrap();
    assert_eq!(
        store.handoff(&handoff.handoff_id).await.unwrap(),
        Some(handoff.clone())
    );
    assert!(matches!(
        store
            .create_handoff(&handoff, &membership, &topology, 50)
            .await
            .unwrap_err(),
        SessionStoreError::HandoffConflict {
            expected: None,
            actual: Some(0),
            ..
        }
    ));

    let awaiting = store
        .transition_handoff(
            &handoff.handoff_id,
            &AgentInstanceId::new("worker-1"),
            HandoffState::AwaitingArbitration,
            0,
            60,
        )
        .await
        .unwrap();
    assert_eq!(awaiting.revision, 1);
    let accepted = store
        .transition_handoff(
            &handoff.handoff_id,
            &AgentInstanceId::new("moderator-1"),
            HandoffState::Accepted,
            1,
            70,
        )
        .await
        .unwrap();
    assert_eq!(accepted.state, HandoffState::Accepted);
    let reassigned = store.task(&task.task_id).await.unwrap().unwrap();
    assert_eq!(
        reassigned.assigned_to,
        Some(AgentInstanceId::new("coordinator-1"))
    );
    assert_eq!(reassigned.handoff_count, 1);
    assert_eq!(reassigned.revision, 1);
    assert!(matches!(
        store
            .transition_handoff(
                &handoff.handoff_id,
                &AgentInstanceId::new("moderator-1"),
                HandoffState::Accepted,
                1,
                71,
            )
            .await
            .unwrap_err(),
        SessionStoreError::HandoffConflict {
            expected: Some(1),
            actual: Some(2),
            ..
        }
    ));
}

#[tokio::test]
async fn mailbox_message_is_durable_and_deduplicated_before_delivery() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    let topology = SessionTopology::new(
        SessionId::new("multi-session"),
        0,
        0,
        vec![
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("worker-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("coordinator-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
        ],
        11,
        &membership,
    )
    .unwrap();
    store
        .save_topology(&topology, &membership, None)
        .await
        .unwrap();
    let message = CoordinationMessage {
        message_id: CoordinationMessageId::new("message-1"),
        session_id: SessionId::new("multi-session"),
        sender_instance_id: AgentInstanceId::new("worker-1"),
        recipient_instance_id: AgentInstanceId::new("coordinator-1"),
        task_id: None,
        kind: CoordinationMessageKind::Evidence,
        payload: "content-free result digest".into(),
        topology_revision: 0,
        route: topology
            .route_between(
                &AgentInstanceId::new("worker-1"),
                &AgentInstanceId::new("coordinator-1"),
            )
            .unwrap(),
        max_hops: 4,
        state: MessageDeliveryState::Pending,
        delivery_attempts: 0,
        revision: 0,
        expires_at: 100,
        created_at: 12,
        updated_at: 12,
    };

    store
        .enqueue_message(&message, &membership, &topology, 50)
        .await
        .unwrap();
    assert_eq!(
        store.message(&message.message_id).await.unwrap(),
        Some(message.clone())
    );
    assert!(matches!(
        store
            .enqueue_message(&message, &membership, &topology, 50)
            .await
            .unwrap_err(),
        SessionStoreError::MessageConflict {
            expected: None,
            actual: Some(0),
            ..
        }
    ));

    let first_claim = store
        .claim_message(&AgentInstanceId::new("coordinator-1"), 50, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.lease_epoch, 1);
    assert!(
        store
            .claim_message(&AgentInstanceId::new("coordinator-1"), 55, 10)
            .await
            .unwrap()
            .is_none()
    );
    let recovered_claim = store
        .claim_message(&AgentInstanceId::new("coordinator-1"), 60, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered_claim.lease_epoch, 2);
    assert!(matches!(
        store
            .finish_message_claim(
                &message.message_id,
                &AgentInstanceId::new("coordinator-1"),
                first_claim.lease_epoch,
                MessageDeliveryState::Delivered,
                61,
            )
            .await
            .unwrap_err(),
        SessionStoreError::MessageConflict { .. }
    ));
    let delivered = store
        .finish_message_claim(
            &message.message_id,
            &AgentInstanceId::new("coordinator-1"),
            recovered_claim.lease_epoch,
            MessageDeliveryState::Delivered,
            61,
        )
        .await
        .unwrap();
    assert_eq!(delivered.state, MessageDeliveryState::Delivered);
    assert_eq!(delivered.delivery_attempts, 2);
    assert_eq!(delivered.revision, 3);
    let acknowledged = store
        .acknowledge_message(
            &message.message_id,
            &AgentInstanceId::new("coordinator-1"),
            delivered.revision,
            62,
        )
        .await
        .unwrap();
    assert_eq!(acknowledged.state, MessageDeliveryState::Acknowledged);
    assert_eq!(acknowledged.revision, 4);
    assert!(matches!(
        store
            .acknowledge_message(
                &message.message_id,
                &AgentInstanceId::new("worker-1"),
                delivered.revision,
                63,
            )
            .await
            .unwrap_err(),
        SessionStoreError::MessageConflict {
            expected: Some(3),
            actual: Some(4),
            ..
        }
    ));
}
