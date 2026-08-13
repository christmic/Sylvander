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
    let review = coordinator.prepare_review(&active.view_id).await.unwrap();
    let integration = coordinator
        .approve_integration(
            WorkspaceIntegrationId::new("integration-1"),
            &active.view_id,
            &review.review_digest,
            AgentInstanceId::new("moderator"),
            &membership(),
            0,
            11,
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
async fn deterministic_isolated_view_is_idempotent() {
    let repository = repository();
    let state = tempfile::tempdir().unwrap();
    let store = store(repository.path()).await;
    let coordinator = AgentWorkspaceCoordinator::new(worktrees(state.path()), store);
    let view_id = WorkspaceViewId::new("agent:worker");

    let first = coordinator
        .ensure_isolated(
            view_id.clone(),
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
    let replay = coordinator
        .ensure_isolated(
            view_id,
            &membership(),
            AgentInstanceId::new("worker"),
            WorkspaceAccess::ReadWrite,
            "local",
            repository.path(),
            2,
            3,
            11,
        )
        .await
        .unwrap();

    assert_eq!(replay, first);
}

#[tokio::test]
async fn shared_read_only_view_is_durable_and_idempotent() {
    let repository = repository();
    let state = tempfile::tempdir().unwrap();
    let store = store(repository.path()).await;
    let coordinator = AgentWorkspaceCoordinator::new(worktrees(state.path()), store.clone());
    let view_id = WorkspaceViewId::new("agent:reader");

    let first = coordinator
        .ensure_shared_read_only(
            view_id.clone(),
            &membership(),
            AgentInstanceId::new("worker"),
            repository.path(),
            2,
            3,
            10,
        )
        .await
        .unwrap();
    let replay = coordinator
        .ensure_shared_read_only(
            view_id.clone(),
            &membership(),
            AgentInstanceId::new("worker"),
            repository.path(),
            2,
            3,
            11,
        )
        .await
        .unwrap();

    assert_eq!(first.state, WorkspaceViewState::Active);
    assert_eq!(first.effective_workspace, repository.path());
    assert_eq!(replay, first);
    assert_eq!(store.workspace_view(&view_id).await.unwrap(), Some(first));
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
    let review = coordinator.prepare_review(&active.view_id).await.unwrap();
    let integration = coordinator
        .approve_integration(
            WorkspaceIntegrationId::new("integration-conflict"),
            &active.view_id,
            &review.review_digest,
            AgentInstanceId::new("moderator"),
            &membership(),
            0,
            11,
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
                && reason.contains("target diverged")
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

#[tokio::test]
async fn applying_position_recovers_an_exact_merge_receipt_after_restart() {
    let repository = repository();
    let state = tempfile::tempdir().unwrap();
    let store = store(repository.path()).await;
    let worktrees = worktrees(state.path());
    let coordinator = AgentWorkspaceCoordinator::new(worktrees.clone(), store.clone());
    let active = coordinator
        .provision(
            WorkspaceViewId::new("view-recover-merge"),
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
    fs::write(
        active.effective_workspace.join("tracked.txt"),
        "recovered\n",
    )
    .unwrap();
    let review = coordinator.prepare_review(&active.view_id).await.unwrap();
    let integration = coordinator
        .approve_integration(
            WorkspaceIntegrationId::new("integration-recover-merge"),
            &active.view_id,
            &review.review_digest,
            AgentInstanceId::new("moderator"),
            &membership(),
            0,
            11,
        )
        .await
        .unwrap();
    let (applying, integrating) = store
        .advance_workspace_integration(
            &integration.approval.integration_id,
            integration.revision,
            active.revision,
            active.lease_epoch,
            active.fencing_token,
            WorkspaceIntegrationState::Applying,
            WorkspaceViewState::Integrating,
            None,
            12,
        )
        .await
        .unwrap();
    let merge_revision = worktrees
        .merge_integration(
            &workspace_lease_id(&active.view_id),
            active.target_id.as_deref(),
            &integration.approval.target_revision,
            &integration.approval.candidate_revision,
        )
        .await
        .unwrap();
    drop(coordinator);

    let recovered = AgentWorkspaceCoordinator::new(worktrees, store.clone())
        .apply_integration(&integration.approval.integration_id, 13)
        .await
        .unwrap();
    let WorkspaceIntegrationOutcome::Applied(recovered) = recovered else {
        panic!("exact merge receipt must recover as applied");
    };
    assert_eq!(applying.state, WorkspaceIntegrationState::Applying);
    assert_eq!(integrating.state, WorkspaceViewState::Integrating);
    assert_eq!(
        recovered.merge_revision.as_deref(),
        Some(merge_revision.as_str())
    );
    assert_eq!(recovered.state, WorkspaceIntegrationState::Applied);
    assert_eq!(
        fs::read_to_string(repository.path().join("tracked.txt")).unwrap(),
        "recovered\n"
    );
}
