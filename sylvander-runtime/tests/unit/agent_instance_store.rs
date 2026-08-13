use std::path::PathBuf;

use crate::agent::instance::{
    AgentDefinitionKey, AgentInstanceOrigin, ApprovalRoute, HistoryView, SessionAgentRole,
};
use crate::coordination::handoff::{HandoffState, TaskHandoff};
use crate::coordination::task::{CoordinationTask, CoordinationTaskState};
use crate::coordination::topology::{AgentRelation, AgentRelationKind, SessionTopology};
use crate::session::SessionMetadata;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::{SessionLifetime, SessionStore, StoredSession};
use sylvander_api::{AgentId, AgentInstanceId, HandoffId, SwarmId, TaskId};

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
