use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::*;
use crate::execution_context::AgentExecutionContext;
use crate::workspace_executor::{
    WorkspaceCommandOutput, WorkspaceExecutor, WorkspaceExecutorError, WorkspaceTarget,
};

#[derive(Debug, Default)]
struct RecordingGitExecutor {
    commands: Mutex<Vec<String>>,
}

#[async_trait]
impl WorkspaceExecutor for RecordingGitExecutor {
    async fn read_file(
        &self,
        _target: &WorkspaceTarget,
        _relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        unreachable!("Git operations do not read through the file port")
    }

    async fn write_file(
        &self,
        _target: &WorkspaceTarget,
        _relative_path: &str,
        _content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        unreachable!("Git operations do not write through the file port")
    }

    async fn run_command(
        &self,
        _target: &WorkspaceTarget,
        _command: &str,
        _timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        unreachable!("Git uses only the structured read-only command port")
    }

    async fn run_read_only_command(
        &self,
        _target: &WorkspaceTarget,
        command: &str,
        _timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        self.commands.lock().unwrap().push(command.to_owned());
        Ok(WorkspaceCommandOutput {
            success: true,
            status_code: Some(0),
            stdout: b"?? new.txt\n".to_vec(),
            stderr: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_total_bytes: 11,
            stderr_total_bytes: 0,
        })
    }
}

fn context(executor: Arc<dyn WorkspaceExecutor>) -> ToolContext {
    ToolContext::new(AgentExecutionContext::restricted_for(
        "user", "agent", "session",
    ))
    .with_executor(executor, WorkspaceTarget::local("/logical/workspace", true))
    .with_capability(Cap::Read)
    .with_capability(Cap::Git)
}

#[tokio::test]
async fn status_uses_the_structured_read_only_port_and_disables_fsmonitor() {
    let executor = Arc::new(RecordingGitExecutor::default());
    let output = GitTool::new()
        .execute(&context(executor.clone()), json!({"operation": "status"}))
        .await
        .unwrap();

    assert!(!output.is_error, "{}", output.content);
    assert!(output.content.contains("?? new.txt"));
    let commands = executor.commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert!(commands[0].contains("-c core.fsmonitor=false"));
    assert!(commands[0].contains("status --short"));
}

#[tokio::test]
async fn diff_rejects_shell_arguments_and_parent_paths_before_execution() {
    let executor = Arc::new(RecordingGitExecutor::default());
    let tool = GitTool::new();
    let context = context(executor.clone());

    assert!(
        tool.execute(
            &context,
            json!({"operation": "diff", "args": ["--exec-path"]}),
        )
        .await
        .is_err()
    );
    assert!(
        tool.execute(&context, json!({"operation": "diff", "path": "../x"}))
            .await
            .is_err()
    );
    assert!(executor.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn requires_both_read_and_git_capabilities() {
    let executor = Arc::new(RecordingGitExecutor::default());
    let base = ToolContext::new(AgentExecutionContext::restricted_for(
        "user", "agent", "session",
    ))
    .with_executor(executor.clone(), WorkspaceTarget::local("/logical", true));
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
    assert!(executor.commands.lock().unwrap().is_empty());
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
