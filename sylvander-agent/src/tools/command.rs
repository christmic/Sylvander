//! Workspace-scoped command execution for coding tasks.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};
use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::{Cap, ToolContext};

const DEFAULT_TIMEOUT: Duration = Duration::from_mins(2);
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

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
        let output = match ctx.executor.run_command(&target, command, timeout).await {
            Ok(output) => output,
            Err(crate::workspace_executor::WorkspaceExecutorError::Timeout(timeout)) => {
                return Err(ToolError::Timeout(timeout));
            }
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
        };

        let stdout = bounded_text(&output.stdout);
        let stderr = bounded_text(&output.stderr);
        let status = output
            .status_code
            .map_or_else(|| "signal".to_string(), |code| code.to_string());
        let content = format!("exit: {status}\nstdout:\n{stdout}\nstderr:\n{stderr}");
        if output.success {
            Ok(ToolOutput::ok(content))
        } else {
            Ok(ToolOutput::err(content))
        }
    }
}

fn bounded_text(bytes: &[u8]) -> String {
    let truncated = bytes.len() > MAX_OUTPUT_BYTES;
    let bytes = &bytes[..bytes.len().min(MAX_OUTPUT_BYTES)];
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if truncated {
        text.push_str("\n[output truncated]");
    }
    text
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
        assert!(output.content.contains("exit: 0"));
        assert!(output.content.contains("hello"));
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
        assert!(output.content.contains("exit: 7"));
        assert!(output.content.contains("boom"));
    }
}
