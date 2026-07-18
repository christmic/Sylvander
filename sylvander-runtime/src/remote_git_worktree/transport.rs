//! Bounded Git command adapter used by the remote worktree manager.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sylvander_agent::workspace_executor::{
    WorkspaceCommandOutput, WorkspaceExecutor, WorkspaceTarget,
};

const OPERATION_TIMEOUT: Duration = Duration::from_mins(2);

#[derive(Clone)]
pub(super) struct RemoteGitTransport {
    target_id: String,
    executor: Arc<dyn WorkspaceExecutor>,
}

impl RemoteGitTransport {
    pub(super) fn new(target_id: String, executor: Arc<dyn WorkspaceExecutor>) -> Self {
        Self {
            target_id,
            executor,
        }
    }

    pub(super) async fn text(&self, workspace: &Path, args: &[&str]) -> Result<String, String> {
        let bytes = self.bytes(workspace, args, &[0]).await?;
        String::from_utf8(bytes)
            .map(|text| text.trim_end().to_owned())
            .map_err(|_| "remote Git output is not valid UTF-8".into())
    }

    pub(super) async fn ok(
        &self,
        workspace: &Path,
        args: &[&str],
        mutable: bool,
    ) -> Result<(), String> {
        self.command_ok(workspace, &git_command(args), mutable)
            .await
    }

    pub(super) async fn bytes(
        &self,
        workspace: &Path,
        args: &[&str],
        allowed: &[i32],
    ) -> Result<Vec<u8>, String> {
        let output = self.command(workspace, &git_command(args), false).await?;
        output_bytes(output, allowed)
    }

    pub(super) async fn status(&self, workspace: &Path, args: &[&str]) -> Result<i32, String> {
        let output = self.command(workspace, &git_command(args), false).await?;
        if output.stdout_truncated || output.stderr_truncated {
            return Err("remote Git output exceeded the execution limit".into());
        }
        output
            .status_code
            .ok_or_else(|| "remote Git process ended without a status code".into())
    }

    pub(super) async fn command_ok(
        &self,
        workspace: &Path,
        command: &str,
        mutable: bool,
    ) -> Result<(), String> {
        let output = self.command(workspace, command, mutable).await?;
        output_bytes(output, &[0]).map(|_| ())
    }

    async fn command(
        &self,
        workspace: &Path,
        command: &str,
        mutable: bool,
    ) -> Result<WorkspaceCommandOutput, String> {
        let target = WorkspaceTarget {
            id: self.target_id.clone(),
            workspace_path: workspace.to_path_buf(),
            read_only: !mutable,
        };
        let result = if mutable {
            self.executor
                .run_command(&target, command, OPERATION_TIMEOUT)
                .await
        } else {
            self.executor
                .run_read_only_command(&target, command, OPERATION_TIMEOUT)
                .await
        };
        result.map_err(|_| "remote worktree transport failed".into())
    }
}

fn output_bytes(output: WorkspaceCommandOutput, allowed: &[i32]) -> Result<Vec<u8>, String> {
    if output.stdout_truncated || output.stderr_truncated {
        return Err("remote Git output exceeded the execution limit".into());
    }
    let status = output
        .status_code
        .ok_or_else(|| "remote Git process ended without a status code".to_string())?;
    if !allowed.contains(&status) {
        return Err("remote Git operation failed".into());
    }
    Ok(output.stdout)
}

fn git_command(args: &[&str]) -> String {
    let mut command = "git -c core.fsmonitor=false --no-pager".to_owned();
    for argument in args {
        command.push(' ');
        command.push_str(&shell_quote(argument));
    }
    command
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
