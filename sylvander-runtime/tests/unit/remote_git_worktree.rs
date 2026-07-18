use std::collections::HashMap;
use std::fs;
use std::process::Command;
use std::sync::Arc;

use sylvander_agent::workspace_executor::LocalExecutor;

use super::*;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

fn repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().expect("repository");
    git(repository.path(), &["init", "-b", "master"]);
    fs::write(repository.path().join("tracked.txt"), "before\n").expect("seed file");
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

fn manager(state: &tempfile::TempDir) -> RemoteGitWorktreeManager {
    let remote_root = state.path().join("remote worktrees");
    fs::create_dir_all(&remote_root).expect("remote root");
    let remote_root = remote_root.canonicalize().expect("canonical remote root");
    RemoteGitWorktreeManager::new(
        state.path().join("manifests"),
        remote_root,
        "ssh:test",
        Arc::new(LocalExecutor),
    )
    .expect("manager")
}

#[tokio::test]
async fn create_review_accept_and_discard_remote_lease() {
    let repository = repository();
    let repository_path = repository
        .path()
        .canonicalize()
        .expect("canonical repository");
    let state = tempfile::tempdir().expect("state");
    let manager = manager(&state);

    let lease = manager
        .create("session-1", &repository_path)
        .await
        .expect("create");
    assert_ne!(lease.worktree_root, repository_path);
    fs::write(lease.worktree_root.join("tracked.txt"), "after\n").expect("edit tracked");
    fs::write(lease.worktree_root.join("new file.txt"), "new\n").expect("write untracked");

    let review = manager.inspect(&lease).await.expect("inspect");
    assert!(review.status.contains("M tracked.txt"));
    assert!(review.status.contains("new file.txt"));
    assert!(review.patch.contains("+after"));
    assert!(review.patch.contains("+new"));

    manager.accept(&lease).await.expect("accept");
    assert_eq!(
        fs::read_to_string(repository.path().join("tracked.txt")).expect("source"),
        "after\n"
    );
    assert_eq!(
        fs::read_to_string(repository.path().join("new file.txt")).expect("source"),
        "new\n"
    );
    manager.discard(&lease).await.expect("discard");
    assert!(!lease.worktree_root.exists());
    assert!(manager.open("session-1").is_err());
}

#[tokio::test]
async fn restart_reconciliation_retains_only_durable_active_sessions() {
    let repository = repository();
    let repository_path = repository
        .path()
        .canonicalize()
        .expect("canonical repository");
    let state = tempfile::tempdir().expect("state");
    let manager = manager(&state);
    let retained = manager
        .create("retained", &repository_path)
        .await
        .expect("retained");
    let removed = manager
        .create("removed", &repository_path)
        .await
        .expect("removed");

    let active = HashMap::from([(
        retained.session_id.clone(),
        retained.effective_workspace.clone(),
    )]);
    assert_eq!(
        manager.reconcile(&active).await.expect("reconcile"),
        WorktreeReconciliation {
            retained: 1,
            removed: 1,
        }
    );
    assert!(retained.worktree_root.exists());
    assert!(!removed.worktree_root.exists());
    manager.discard(&retained).await.expect("cleanup");
}

#[tokio::test]
async fn reconciliation_cleans_creation_intent_left_before_activation() {
    let repository = repository();
    let repository_path = repository
        .path()
        .canonicalize()
        .expect("canonical repository");
    let state = tempfile::tempdir().expect("state");
    let manager = manager(&state);
    let mut lease = manager
        .create("interrupted", &repository_path)
        .await
        .expect("create");
    lease.state = RemoteLeaseState::Creating;
    manager.manifests.save(&lease).expect("persist intent");

    assert_eq!(
        manager.reconcile(&HashMap::new()).await.expect("reconcile"),
        WorktreeReconciliation {
            retained: 0,
            removed: 1,
        }
    );
    assert!(!lease.worktree_root.exists());
    assert!(manager.open("interrupted").is_err());
}

#[tokio::test]
async fn dirty_source_and_unsafe_lease_identity_fail_before_creation() {
    let repository = repository();
    let repository_path = repository
        .path()
        .canonicalize()
        .expect("canonical repository");
    let state = tempfile::tempdir().expect("state");
    let manager = manager(&state);

    assert!(manager.create("../escape", &repository_path).await.is_err());
    fs::write(repository.path().join("dirty.txt"), "dirty").expect("dirty source");
    let error = manager
        .create("safe-session", &repository_path)
        .await
        .expect_err("dirty source rejected");
    assert!(error.contains("uncommitted changes"));
    assert!(!state.path().join("remote worktrees/safe-session").exists());
}

#[tokio::test]
async fn concurrent_session_creation_produces_distinct_leases() {
    let repository = repository();
    let repository_path = repository
        .path()
        .canonicalize()
        .expect("canonical repository");
    let state = tempfile::tempdir().expect("state");
    let manager = manager(&state);
    let first = manager.create("one", &repository_path);
    let second = manager.create("two", &repository_path);
    let (first, second) = tokio::join!(first, second);
    let first = first.expect("first");
    let second = second.expect("second");
    assert_ne!(first.branch, second.branch);
    assert_ne!(first.worktree_root, second.worktree_root);
    manager.discard(&first).await.expect("discard first");
    manager.discard(&second).await.expect("discard second");
}
