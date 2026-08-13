use std::path::PathBuf;

use crate::agent::instance::{
    AgentDefinitionKey, AgentInstanceOrigin, ApprovalRoute, HistoryView, SessionAgentRole,
};
use crate::session::SessionMetadata;
use crate::storage::session::{SessionLifetime, SessionStore, StoredSession};
use sylvander_api::{AgentId, AgentInstanceId, SwarmId};

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
