use std::path::PathBuf;

use crate::agent::instance::{
    AgentDefinitionKey, AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute,
    HistoryView, SessionAgentRole,
};
use crate::coordination::topology::{AgentRelation, AgentRelationKind, SessionTopology};
use crate::session::SessionMetadata;
use crate::session::membership::{SessionGovernance, SessionMembership};
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::{SessionLifetime, SessionStore, StoredSession};
use sylvander_api::AgentId;

use super::*;

fn membership() -> SessionMembership {
    let session_id = SessionId::new("session");
    let instance = |id: &str, role| AgentInstance {
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
            instance("moderator", SessionAgentRole::Moderator),
            instance("worker", SessionAgentRole::Worker),
        ],
        SessionGovernance {
            session_id,
            moderator_instance_id: AgentInstanceId::new("moderator"),
            governance_revision: "sha256:governance".into(),
            membership_revision: 0,
            lease_epoch: 1,
            fencing_token: 1,
            updated_at: 1,
        },
    )
    .unwrap()
}

fn workspace_view(id: &str) -> AgentWorkspaceView {
    AgentWorkspaceView {
        view_id: WorkspaceViewId::new(id),
        session_id: SessionId::new("session"),
        agent_instance_id: AgentInstanceId::new("worker"),
        membership_revision: 0,
        access: WorkspaceAccess::ReadWrite,
        isolation: WorkspaceIsolation::IsolatedWorktree,
        source_workspace: PathBuf::from("/repo"),
        effective_workspace: PathBuf::from(format!("/leases/{id}")),
        target_id: Some("local".into()),
        branch: Some(format!("sylvander/{id}")),
        base_revision: Some("abc123".into()),
        state: WorkspaceViewState::Provisioning,
        lease_epoch: 2,
        fencing_token: 3,
        revision: 0,
        created_at: 2,
        updated_at: 2,
    }
}

#[tokio::test]
async fn workspace_view_is_durable_unique_and_fenced() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store
        .save(&StoredSession::new(
            SessionId::new("session"),
            "session",
            SessionLifetime::Persistent,
            SessionMetadata {
                workspace: PathBuf::from("/repo"),
                name: "session".into(),
                user_id: "user".into(),
            },
            vec![AgentId::new("moderator")],
        ))
        .await
        .unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    let topology = SessionTopology::new(
        membership.session_id.clone(),
        0,
        0,
        vec![AgentRelation {
            source: AgentInstanceId::new("moderator"),
            target: AgentInstanceId::new("worker"),
            kind: AgentRelationKind::ParentOf,
            created_at: 1,
        }],
        1,
        &membership,
    )
    .unwrap();
    store
        .save_topology(&topology, &membership, None)
        .await
        .unwrap();
    let view = workspace_view("view-1");
    store
        .create_workspace_view(&view, &membership)
        .await
        .unwrap();
    assert_eq!(
        store.workspace_view(&view.view_id).await.unwrap(),
        Some(view.clone())
    );
    assert!(matches!(
        store
            .create_workspace_view(&workspace_view("view-2"), &membership)
            .await
            .unwrap_err(),
        SessionStoreError::Invalid(reason) if reason.contains("active workspace")
    ));

    let active = store
        .transition_workspace_view(&view.view_id, 0, 2, 3, WorkspaceViewState::Active, 3)
        .await
        .unwrap();
    assert_eq!(active.revision, 1);
    assert_eq!(
        store
            .active_workspace_views(&view.session_id)
            .await
            .unwrap(),
        std::slice::from_ref(&active)
    );
    let approval = WorkspaceIntegrationApproval {
        integration_id: WorkspaceIntegrationId::new("integration-1"),
        view_id: active.view_id.clone(),
        session_id: active.session_id.clone(),
        agent_instance_id: active.agent_instance_id.clone(),
        approved_by: AgentInstanceId::new("moderator"),
        membership_revision: 0,
        topology_revision: 0,
        view_revision: 1,
        lease_epoch: 2,
        fencing_token: 3,
        review_digest: "sha256:review".into(),
        approved_at: 4,
    };
    let integration = WorkspaceIntegration::new(approval, &active, &membership, 0).unwrap();
    store
        .create_workspace_integration(&integration, &active, &membership, 0)
        .await
        .unwrap();
    assert_eq!(
        store
            .workspace_integration(&integration.approval.integration_id)
            .await
            .unwrap(),
        Some(integration.clone())
    );
    let applying = store
        .transition_workspace_integration(
            &integration.approval.integration_id,
            0,
            WorkspaceIntegrationState::Applying,
            5,
        )
        .await
        .unwrap();
    assert_eq!(applying.state, WorkspaceIntegrationState::Applying);
    assert!(matches!(
        store
            .transition_workspace_view(
                &view.view_id,
                1,
                2,
                2,
                WorkspaceViewState::Integrating,
                4,
            )
            .await
            .unwrap_err(),
        SessionStoreError::Invalid(reason) if reason.contains("superseded")
    ));
}
