//! Workspace-scoped command execution for coding tasks.

use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};
use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::{Cap, ToolContext};

const DEFAULT_TIMEOUT: Duration = Duration::from_mins(2);
// Tool results enter both the model context and the TUI transcript. Keep the
// final structured payload compact; the executor retains a larger bounded
// head/tail capture for diagnostics.
const MAX_MODEL_BYTES_PER_STREAM: usize = 24 * 1024;

/// Run a shell command inside the invocation's effective workspace.
#[derive(Debug, Clone)]
pub struct CommandTool {
    workdir: PathBuf,
}

impl CommandTool {
    #[must_use]
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

#[async_trait]
impl Tool for CommandTool {
    fn name(&self) -> &'static str {
        "Command"
    }

    fn description(&self) -> &'static str {
        "Run a shell command in the current workspace. Use it for builds, tests, formatting, search, and git inspection. Returns exit status, stdout, and stderr."
    }

    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            json!({"command": {
                "type": "string",
                "description": "Shell command to run"
            }}),
            &["command"],
        )
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(Cap::Spawn) {
            return Ok(ToolOutput::err(
                "command execution is not enabled for this workspace",
            ));
        }
        let command = input
            .get("command")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ToolError::Other("missing required field `command`".into()))?;
        let target = ctx.execution_target_for(&self.workdir);
        if target.read_only {
            return Ok(ToolOutput::err(format!(
                "execution target `{}` is read-only",
                target.id
            )));
        }
        let timeout = ctx.budget.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let started = Instant::now();
        let output = match ctx.executor.run_command(&target, command, timeout).await {
            Ok(output) => output,
            Err(crate::workspace_executor::WorkspaceExecutorError::Timeout(timeout)) => {
                return Err(ToolError::Timeout(timeout));
            }
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
        };

        let (stdout, stdout_model_truncated) = model_text(&output.stdout);
        let (stderr, stderr_model_truncated) = model_text(&output.stderr);
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let content = serde_json::to_string(&json!({
            "exit_code": output.status_code,
            "duration_ms": duration_ms,
            "stdout": stdout,
            "stderr": stderr,
            "stdout_total_bytes": output.stdout_total_bytes,
            "stderr_total_bytes": output.stderr_total_bytes,
            "stdout_truncated": output.stdout_truncated || stdout_model_truncated,
            "stderr_truncated": output.stderr_truncated || stderr_model_truncated,
        }))
        .expect("command result is serializable");
        if output.success {
            Ok(ToolOutput::ok(content))
        } else {
            Ok(ToolOutput::err(content))
        }
    }
}

fn model_text(bytes: &[u8]) -> (String, bool) {
    if bytes.len() <= MAX_MODEL_BYTES_PER_STREAM {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    let head_bytes = MAX_MODEL_BYTES_PER_STREAM / 4;
    let tail_bytes = MAX_MODEL_BYTES_PER_STREAM - head_bytes;
    let head = String::from_utf8_lossy(&bytes[..head_bytes]);
    let tail = String::from_utf8_lossy(&bytes[bytes.len() - tail_bytes..]);
    (format!("{head}\n[… output omitted …]\n{tail}"), true)
}

#[cfg(test)]
mod tests {
    use sylvander_protocol::{AgentId, SessionContext, SessionId, UserId};

    use super::*;

    fn context(root: &std::path::Path) -> ToolContext {
        ToolContext::new(SessionContext::new(
            UserId::new("user"),
            AgentId::new("agent"),
            SessionId::new("session"),
        ))
        .with_fs_root(root)
        .with_capability(Cap::Spawn)
    }

    #[tokio::test]
    async fn runs_in_effective_workspace_and_reports_status() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello\n").unwrap();
        let output = CommandTool::new("/")
            .execute(&context(dir.path()), json!({"command": "cat hello.txt"}))
            .await
            .unwrap();
        assert!(!output.is_error);
        let result: JsonValue = serde_json::from_str(&output.content).unwrap();
        assert_eq!(result["exit_code"], 0);
        assert_eq!(result["stdout"], "hello\n");
        assert_eq!(result["stdout_truncated"], false);
    }

    #[tokio::test]
    async fn non_zero_exit_is_visible_to_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let output = CommandTool::new("/")
            .execute(
                &context(dir.path()),
                json!({"command": "printf boom >&2; exit 7"}),
            )
            .await
            .unwrap();
        assert!(output.is_error);
        let result: JsonValue = serde_json::from_str(&output.content).unwrap();
        assert_eq!(result["exit_code"], 7);
        assert_eq!(result["stderr"], "boom");
        assert_eq!(result["stderr_truncated"], false);
    }
}
