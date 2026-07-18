use std::fs;
use std::process::Command;

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

#[tokio::test]
async fn local_target_lifecycle_uses_one_transport_neutral_contract() {
    let repository = repository();
    let state = tempfile::tempdir().expect("state");
    let mut service = CodingWorktreeService::new(Arc::new(GitWorktreeManager::new(state.path())));
    service.register_local("local").expect("local target");

    let lease = service
        .create("session-1", "local", repository.path())
        .await
        .expect("create")
        .expect("Git workspace");
    assert_eq!(lease.target_id, None);
    fs::write(lease.effective_workspace.join("tracked.txt"), "after\n").expect("edit");

    let review = service
        .inspect("session-1", lease.target_id.as_deref())
        .await
        .expect("inspect");
    assert!(review.status.contains("M tracked.txt"));
    assert!(review.patch.contains("+after"));

    service
        .accept("session-1", lease.target_id.as_deref())
        .await
        .expect("accept");
    assert_eq!(
        fs::read_to_string(repository.path().join("tracked.txt")).expect("source"),
        "after\n"
    );
    service
        .discard("session-1", lease.target_id.as_deref())
        .await
        .expect("discard");
}

#[tokio::test]
async fn unknown_target_never_falls_back_to_server_filesystem() {
    let repository = repository();
    let state = tempfile::tempdir().expect("state");
    let service = CodingWorktreeService::new(Arc::new(GitWorktreeManager::new(state.path())));

    let error = service
        .create("session-1", "missing", repository.path())
        .await
        .expect_err("unknown target rejected");
    assert!(error.contains("unknown coding worktree target"));
    assert!(!state.path().join("worktrees/session-1").exists());
}

#[tokio::test]
async fn writable_remote_non_git_workspace_is_rejected_without_fallback() {
    let workspace = tempfile::tempdir().expect("non-Git workspace");
    let remote_root = tempfile::tempdir().expect("remote worktree root");
    let state = tempfile::tempdir().expect("state");
    let mut service = CodingWorktreeService::new(Arc::new(GitWorktreeManager::new(state.path())));
    service
        .register_remote(
            "ssh:dev",
            Arc::new(
                RemoteGitWorktreeManager::new(
                    state.path().join("remote"),
                    remote_root.path(),
                    "ssh:dev",
                    Arc::new(sylvander_agent::workspace_executor::LocalExecutor),
                )
                .expect("remote manager"),
            ),
        )
        .expect("remote target");

    let error = service
        .create("session-remote", "ssh:dev", workspace.path())
        .await
        .expect_err("remote non-Git mutation must fail closed");
    assert_eq!(
        error,
        "writable remote workspace requires a Git worktree transaction"
    );
    assert!(!state.path().join("worktrees/session-remote").exists());
}
