use std::fs;
use std::process::Command;

use crate::agent::instance::{
    AgentDefinitionKey, AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute,
    HistoryView, SessionAgentRole,
};
use crate::coordination::topology::{AgentRelation, AgentRelationKind, SessionTopology};
use crate::session::SessionMetadata;
use crate::session::membership::SessionGovernance;
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::{SessionLifetime, SessionStore, SqliteSessionStore, StoredSession};
use crate::storage::workspace_coordination::AgentWorkspaceStore;
use crate::workspace::local::GitWorktreeManager;
use sylvander_api::{AgentId, SessionId, WorkspaceIntegrationId};

use super::*;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    git(repository.path(), &["init", "-b", "master"]);
    fs::write(repository.path().join("tracked.txt"), "seed\n").unwrap();
    git(repository.path(), &["add", "."]);
    git(
        repository.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial",
        ],
    );
    repository
}

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

async fn store(repository: &Path) -> Arc<SqliteSessionStore> {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store
        .save(&StoredSession::new(
            SessionId::new("session"),
            "session",
            SessionLifetime::Persistent,
            SessionMetadata {
                workspace: repository.to_owned(),
                name: "session".into(),
                user_id: "user".into(),
            },
            vec![AgentId::new("moderator")],
        ))
        .await
        .unwrap();
    store
        .save_session_membership(&membership(), None)
        .await
        .unwrap();
    let membership = membership();
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
    store
}

fn worktrees(state: &Path) -> Arc<CodingWorktreeService> {
    let mut worktrees = CodingWorktreeService::new(Arc::new(GitWorktreeManager::new(state)));
    worktrees.register_local("local").unwrap();
    Arc::new(worktrees)
}

#[tokio::test]
async fn provisioning_commits_exact_worktree_receipt_before_activation() {
    let repository = repository();
    let state = tempfile::tempdir().unwrap();
    let store = store(repository.path()).await;
    let coordinator = AgentWorkspaceCoordinator::new(worktrees(state.path()), store.clone());

    let active = coordinator
        .provision(
            WorkspaceViewId::new("view-1"),
            &membership(),
            AgentInstanceId::new("worker"),
            WorkspaceAccess::ReadWrite,
            "local",
            repository.path(),
            2,
            3,
            10,
        )
        .await
        .unwrap();

    assert_eq!(active.state, WorkspaceViewState::Active);
    assert_eq!(active.revision, 1);
    assert_ne!(active.source_workspace, active.effective_workspace);
    fs::write(
        active.effective_workspace.join("tracked.txt"),
        "integrated\n",
    )
    .unwrap();
    let integration = coordinator
        .approve_integration(
            WorkspaceIntegrationApproval {
                integration_id: WorkspaceIntegrationId::new("integration-1"),
                view_id: active.view_id.clone(),
                session_id: active.session_id.clone(),
                agent_instance_id: active.agent_instance_id.clone(),
                approved_by: AgentInstanceId::new("moderator"),
                membership_revision: 0,
                topology_revision: 0,
                view_revision: active.revision,
                lease_epoch: active.lease_epoch,
                fencing_token: active.fencing_token,
                review_digest: "sha256:reviewed".into(),
                target_revision: active.base_revision.clone().unwrap(),
                approved_at: 11,
            },
            &membership(),
            0,
        )
        .await
        .unwrap();
    assert!(matches!(
        coordinator
            .apply_integration(&integration.approval.integration_id, 12)
            .await
            .unwrap(),
        WorkspaceIntegrationOutcome::Applied(_)
    ));
    assert_eq!(
        fs::read_to_string(repository.path().join("tracked.txt")).unwrap(),
        "integrated\n"
    );
    assert_eq!(
        store
            .workspace_view(&active.view_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        WorkspaceViewState::Integrated
    );
}

#[tokio::test]
async fn durable_provisioning_position_recovers_from_matching_receipt() {
    let repository = repository();
    let state = tempfile::tempdir().unwrap();
    let store = store(repository.path()).await;
    let worktrees = worktrees(state.path());
    let lease = worktrees
        .create("view-2", "local", repository.path())
        .await
        .unwrap()
        .unwrap();
    let view = workspace_view(
        WorkspaceViewId::new("view-2"),
        &membership(),
        AgentInstanceId::new("worker"),
        WorkspaceAccess::ReadWrite,
        repository.path(),
        &lease,
        2,
        3,
        10,
    );
    store
        .create_workspace_view(&view, &membership())
        .await
        .unwrap();
    let coordinator = AgentWorkspaceCoordinator::new(worktrees, store.clone());

    let recovered = coordinator.recover_provisioning(&view).await.unwrap();

    assert_eq!(recovered.state, WorkspaceViewState::Active);
    assert_eq!(
        store.workspace_view(&view.view_id).await.unwrap(),
        Some(recovered)
    );
}

#[tokio::test]
async fn target_advance_after_review_is_fenced_as_conflict_without_merge() {
    let repository = repository();
    let state = tempfile::tempdir().unwrap();
    let store = store(repository.path()).await;
    let coordinator = AgentWorkspaceCoordinator::new(worktrees(state.path()), store.clone());
    let active = coordinator
        .provision(
            WorkspaceViewId::new("view-conflict"),
            &membership(),
            AgentInstanceId::new("worker"),
            WorkspaceAccess::ReadWrite,
            "local",
            repository.path(),
            2,
            3,
            10,
        )
        .await
        .unwrap();
    fs::write(active.effective_workspace.join("tracked.txt"), "agent\n").unwrap();
    let integration = coordinator
        .approve_integration(
            WorkspaceIntegrationApproval {
                integration_id: WorkspaceIntegrationId::new("integration-conflict"),
                view_id: active.view_id.clone(),
                session_id: active.session_id.clone(),
                agent_instance_id: active.agent_instance_id.clone(),
                approved_by: AgentInstanceId::new("moderator"),
                membership_revision: 0,
                topology_revision: 0,
                view_revision: active.revision,
                lease_epoch: active.lease_epoch,
                fencing_token: active.fencing_token,
                review_digest: "sha256:reviewed-before-target-change".into(),
                target_revision: active.base_revision.clone().unwrap(),
                approved_at: 11,
            },
            &membership(),
            0,
        )
        .await
        .unwrap();
    fs::write(repository.path().join("tracked.txt"), "other\n").unwrap();
    git(repository.path(), &["add", "."]);
    git(
        repository.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "advance target",
        ],
    );

    let outcome = coordinator
        .apply_integration(&integration.approval.integration_id, 12)
        .await
        .unwrap();

    assert!(matches!(
        outcome,
        WorkspaceIntegrationOutcome::Conflicted { integration, reason }
            if integration.state == WorkspaceIntegrationState::Conflicted
                && reason.contains("target advanced")
    ));
    assert_eq!(
        fs::read_to_string(repository.path().join("tracked.txt")).unwrap(),
        "other\n"
    );
    assert!(git(repository.path(), &["status", "--porcelain"]).is_empty());
    assert_eq!(
        store
            .workspace_view(&active.view_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        WorkspaceViewState::Conflicted
    );
}
