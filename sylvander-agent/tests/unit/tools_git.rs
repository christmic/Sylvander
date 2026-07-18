use std::sync::Arc;

use sylvander_protocol::{AgentId, SessionContext, SessionId, UserId};

use super::*;
use crate::workspace_executor::{LocalExecutor, WorkspaceTarget};

fn context(root: &Path, read_only: bool) -> ToolContext {
    ToolContext::new(SessionContext::new(
        UserId::new("user"),
        AgentId::new("agent"),
        SessionId::new("session"),
    ))
    .with_executor(
        Arc::new(LocalExecutor),
        WorkspaceTarget::local(root, read_only),
    )
    .with_capability(Cap::Read)
    .with_capability(Cap::Git)
}

fn init_repository(root: &Path) {
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn status_runs_in_a_read_only_workspace() {
    let dir = tempfile::tempdir().unwrap();
    init_repository(dir.path());
    std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();

    let output = GitTool::new()
        .execute(&context(dir.path(), true), json!({"operation": "status"}))
        .await
        .unwrap();

    assert!(!output.is_error, "{}", output.content);
    assert!(output.content.contains("?? new.txt"));
}

#[cfg(unix)]
#[tokio::test]
async fn status_does_not_execute_repository_fsmonitor() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    init_repository(dir.path());
    let marker = dir.path().join("fsmonitor-executed");
    let monitor = dir.path().join("fsmonitor.sh");
    std::fs::write(
        &monitor,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&monitor).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&monitor, permissions).unwrap();
    let configured = std::process::Command::new("git")
        .args(["config", "core.fsmonitor"])
        .arg(&monitor)
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(configured.success());

    let output = GitTool::new()
        .execute(&context(dir.path(), true), json!({"operation": "status"}))
        .await
        .unwrap();

    assert!(!output.is_error, "{}", output.content);
    assert!(!marker.exists(), "repository fsmonitor was executed");
}

#[tokio::test]
async fn diff_rejects_shell_arguments_and_parent_paths() {
    let dir = tempfile::tempdir().unwrap();
    init_repository(dir.path());
    let tool = GitTool::new();
    let context = context(dir.path(), true);

    let arbitrary = tool
        .execute(
            &context,
            json!({"operation": "diff", "args": ["--exec-path"]}),
        )
        .await;
    let escaped = tool
        .execute(&context, json!({"operation": "diff", "path": "../x"}))
        .await;

    assert!(arbitrary.is_err());
    assert!(escaped.is_err());
}

#[tokio::test]
async fn requires_both_read_and_git_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let base = ToolContext::new(SessionContext::new("user", "agent", "session")).with_executor(
        Arc::new(LocalExecutor),
        WorkspaceTarget::local(dir.path(), true),
    );
    let tool = GitTool::new();

    let read_only = tool
        .execute(
            &base.clone().with_capability(Cap::Read),
            json!({"operation": "status"}),
        )
        .await
        .unwrap();
    let git_only = tool
        .execute(
            &base.with_capability(Cap::Git),
            json!({"operation": "status"}),
        )
        .await
        .unwrap();

    assert!(read_only.is_error);
    assert!(git_only.is_error);
}

#[test]
fn log_is_bounded_and_paths_are_shell_quoted() {
    let command = log_command(
        json!({"operation": "log", "max_count": 5, "path": "it's here.rs"})
            .as_object()
            .unwrap(),
    )
    .unwrap();
    assert!(command.contains("-n 5"));
    assert!(command.ends_with("-- 'it'\\''s here.rs'"));

    let error = log_command(
        json!({"operation": "log", "max_count": 101})
            .as_object()
            .unwrap(),
    );
    assert!(error.is_err());
}
