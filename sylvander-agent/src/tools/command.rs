//! Workspace-scoped command execution for coding tasks.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};
use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput, ToolProgressSink};
use crate::tool_context::{Cap, ToolContext};
use crate::workspace_executor::{WorkspaceCommandProgressSink, WorkspaceCommandStream};

const DEFAULT_TIMEOUT: Duration = Duration::from_mins(2);
// Tool results enter both the model context and the TUI transcript. Keep the
// final structured payload compact; the executor retains a larger bounded
// head/tail capture for diagnostics.
const MAX_MODEL_BYTES_PER_STREAM: usize = 4 * 1024;

/// Run a shell command inside the invocation's effective workspace.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommandTool;

impl CommandTool {
    #[must_use]
    pub const fn new() -> Self {
        Self
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
            json!({
                "command": {
                    "type": "string",
                    "description": "Shell command to run"
                },
                "workspace": {
                    "type": "string",
                    "description": "Optional logical workspace reference without the @ prefix"
                },
                "environment": {
                    "type": "object",
                    "description": "Optional command-scoped environment overrides",
                    "additionalProperties": { "type": "string" }
                }
            }),
            &["command"],
        )
    }

    fn invocation_class(&self) -> crate::tool_invocation::ToolInvocationClass {
        crate::tool_invocation::ToolInvocationClass::Terminal
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        self.execute_inner(ctx, input, None).await
    }

    async fn execute_streaming(
        &self,
        ctx: &ToolContext,
        input: JsonValue,
        progress: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError> {
        self.execute_inner(ctx, input, Some(progress)).await
    }
}

impl CommandTool {
    async fn execute_inner(
        &self,
        ctx: &ToolContext,
        input: JsonValue,
        progress: Option<ToolProgressSink>,
    ) -> Result<ToolOutput, ToolError> {
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
        let workspace = input.get("workspace").and_then(JsonValue::as_str);
        let environment = parse_environment(input.get("environment"))?;
        let base_target = match ctx.require_execution_target() {
            Ok(target) => target,
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
        };
        let target = match ctx.executor.select_mount_target(base_target, workspace) {
            Ok(target) => target,
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
        };
        if target.read_only {
            return Ok(ToolOutput::err(format!(
                "execution target `{}` is read-only",
                target.id
            )));
        }
        let timeout = ctx.budget.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let started = Instant::now();
        let progress_state = progress.map(CommandProgress::new);
        let result = if let Some(state) = &progress_state {
            ctx.executor
                .run_command_streaming_with_environment(
                    &target,
                    command,
                    timeout,
                    &environment,
                    state.executor_sink(),
                )
                .await
        } else {
            ctx.executor
                .run_command_with_environment(&target, command, timeout, &environment)
                .await
        };
        if let Some(state) = &progress_state {
            state.flush();
        }
        let output = match result {
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

fn parse_environment(value: Option<&JsonValue>) -> Result<BTreeMap<String, String>, ToolError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| ToolError::Other("`environment` must be an object of strings".into()))?;
    object
        .iter()
        .map(|(name, value)| {
            value
                .as_str()
                .map(|value| (name.clone(), value.to_owned()))
                .ok_or_else(|| ToolError::Other("`environment` values must be strings".into()))
        })
        .collect()
}

struct CommandProgress {
    sink: ToolProgressSink,
    state: Arc<Mutex<CommandProgressState>>,
}

struct CommandProgressState {
    pending: String,
    last_emit: Instant,
}

impl CommandProgress {
    fn new(sink: ToolProgressSink) -> Self {
        Self {
            sink,
            state: Arc::new(Mutex::new(CommandProgressState {
                pending: String::new(),
                last_emit: Instant::now()
                    .checked_sub(Duration::from_secs(1))
                    .unwrap_or_else(Instant::now),
            })),
        }
    }

    fn executor_sink(&self) -> WorkspaceCommandProgressSink {
        let state = self.state.clone();
        let sink = self.sink.clone();
        WorkspaceCommandProgressSink::new(move |stream, delta| {
            let mut state = state.lock().unwrap();
            if stream == WorkspaceCommandStream::Stderr {
                state.pending.push_str("[stderr] ");
            }
            state.pending.push_str(&delta);
            keep_recent_chars(&mut state.pending, 4_096);
            if state.last_emit.elapsed() >= Duration::from_millis(150) {
                let delta = std::mem::take(&mut state.pending);
                state.last_emit = Instant::now();
                drop(state);
                sink.emit(delta);
            }
        })
    }

    fn flush(&self) {
        let delta = {
            let mut state = self.state.lock().unwrap();
            std::mem::take(&mut state.pending)
        };
        if !delta.is_empty() {
            self.sink.emit(delta);
        }
    }
}

fn keep_recent_chars(value: &mut String, max_chars: usize) {
    let count = value.chars().count();
    if count <= max_chars {
        return;
    }
    *value = format!(
        "… earlier command output omitted …\n{}",
        value.chars().skip(count - max_chars).collect::<String>()
    );
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
#[path = "../../tests/unit/tools_command.rs"]
mod tests;
