//! Opt-in end-to-end OpenSSH executor and remote-worktree journey.
//!
//! The caller owns the disposable remote repository and SSH daemon. This test
//! never weakens host-key verification and never falls back to local paths.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sylvander_agent::workspace_executor::{
    WorkspaceExecutor, WorkspaceExecutorError, WorkspaceTarget,
};
use sylvander_runtime::execution::SshExecutor;
use sylvander_runtime::remote_git_worktree::RemoteGitWorktreeManager;

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}

fn executor() -> SshExecutor {
    SshExecutor::new(
        required("SYLVANDER_TEST_SSH_HOST"),
        required("SYLVANDER_TEST_SSH_PORT")
            .parse()
            .expect("valid SSH port"),
        required("SYLVANDER_TEST_SSH_USER"),
        PathBuf::from(required("SYLVANDER_TEST_SSH_IDENTITY")),
        PathBuf::from(required("SYLVANDER_TEST_SSH_KNOWN_HOSTS")),
        PathBuf::from(required("SYLVANDER_TEST_SSH_CONTROL_PATH")),
    )
    .expect("valid SSH executor")
}

#[tokio::test]
#[ignore = "requires a disposable real SSH daemon and repository"]
async fn real_ssh_executor_worktree_restart_review_accept_and_cancel() {
    let source = PathBuf::from(required("SYLVANDER_TEST_SSH_REPOSITORY"));
    let remote_root = PathBuf::from(required("SYLVANDER_TEST_SSH_WORKTREE_ROOT"));
    let seed_file =
        std::env::var("SYLVANDER_TEST_SSH_SEED_FILE").unwrap_or_else(|_| "tracked.txt".into());
    let local_state = tempfile::tempdir().expect("local manifest directory");
    let executor = Arc::new(executor());
    let manager = RemoteGitWorktreeManager::new(
        local_state.path(),
        &remote_root,
        "ssh:real-smoke",
        executor.clone(),
    )
    .expect("remote manager");
    let session_id = format!("ssh-smoke-{}", uuid::Uuid::new_v4().simple());
    let lease = manager
        .create(&session_id, &source)
        .await
        .expect("create remote worktree");
    let worktree = WorkspaceTarget {
        id: "ssh:real-smoke".into(),
        workspace_path: lease.effective_workspace.clone(),
        read_only: false,
    };

    assert!(
        !executor
            .read_file(&worktree, &seed_file)
            .await
            .expect("read seed")
            .is_empty()
    );
    executor
        .write_file(
            &worktree,
            "sylvander-smoke.txt",
            "你好，远程工作区\n".as_bytes(),
        )
        .await
        .expect("write through SSH");
    let command = executor
        .run_command(
            &worktree,
            "printf 'stream-ok\\n'; git status --short",
            Duration::from_secs(10),
        )
        .await
        .expect("run cancellable SSH command");
    assert!(command.success);
    assert!(String::from_utf8_lossy(&command.stdout).contains("sylvander-smoke.txt"));

    let cancellation = executor
        .run_command(
            &worktree,
            "trap '' HUP; (sleep 1; printf leaked > cancellation-leak.txt) & sleep 30",
            Duration::from_millis(250),
        )
        .await;
    assert!(matches!(
        cancellation,
        Err(WorkspaceExecutorError::Timeout(_))
    ));
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(
        executor
            .read_file(&worktree, "cancellation-leak.txt")
            .await
            .is_err(),
        "timed-out remote descendants survived the SSH cancellation boundary"
    );

    let review = manager.inspect(&lease).await.expect("review");
    assert!(review.status.contains("sylvander-smoke.txt"));
    assert!(review.patch.contains("你好，远程工作区"));
    drop(manager);

    let restarted = RemoteGitWorktreeManager::new(
        local_state.path(),
        remote_root,
        "ssh:real-smoke",
        executor.clone(),
    )
    .expect("restart manager");
    assert_eq!(
        restarted
            .reconcile(&HashMap::from([(
                session_id.clone(),
                lease.effective_workspace.clone(),
            )]))
            .await
            .expect("reconcile after restart")
            .retained,
        1
    );
    restarted
        .accept(&lease)
        .await
        .expect("accept reviewed worktree");
    let source_target = WorkspaceTarget {
        id: "ssh:real-smoke".into(),
        workspace_path: source,
        read_only: true,
    };
    assert_eq!(
        executor
            .read_file(&source_target, "sylvander-smoke.txt")
            .await
            .expect("accepted source"),
        "你好，远程工作区\n".as_bytes()
    );
    restarted
        .discard(&lease)
        .await
        .expect("discard accepted lease");
}
