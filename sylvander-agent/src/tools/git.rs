//! Structured, read-only Git inspection for coding tasks.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::{Cap, ToolContext};
use crate::workspace_executor::WorkspaceExecutorError;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const DEFAULT_LOG_COUNT: u64 = 20;
const MAX_LOG_COUNT: u64 = 100;
const SAFE_GIT_PREFIX: &str = "GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_COUNT=0 GIT_OPTIONAL_LOCKS=0 git -c core.fsmonitor=false";

/// Inspect the Git repository in the invocation's effective workspace.
///
/// Unlike [`super::CommandTool`], this tool accepts a small structured
/// operation set and never accepts an arbitrary command or revision argument.
#[derive(Debug, Clone)]
pub struct GitTool {
    workdir: PathBuf,
}

impl GitTool {
    #[must_use]
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &'static str {
        "Git"
    }

    fn description(&self) -> &'static str {
        "Inspect the current Git repository with structured read-only operations: status, diff, and log. This tool cannot modify the repository."
    }

    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            json!({
                "operation": {
                    "type": "string",
                    "enum": ["status", "diff", "log"],
                    "description": "Read-only Git operation to perform"
                },
                "staged": {
                    "type": "boolean",
                    "description": "For diff only: inspect staged changes"
                },
                "stat": {
                    "type": "boolean",
                    "description": "For diff only: show the summary instead of the patch"
                },
                "path": {
                    "type": "string",
                    "description": "For diff or log only: repository-relative path filter"
                },
                "max_count": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LOG_COUNT,
                    "description": "For log only: maximum number of commits"
                },
                "workspace": {
                    "type": "string",
                    "description": "Optional logical workspace reference without the @ prefix"
                }
            }),
            &["operation"],
        )
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(Cap::Read) || !ctx.has_cap(Cap::Git) {
            return Ok(ToolOutput::err(
                "read-only Git inspection is not enabled for this workspace",
            ));
        }

        let object = input
            .as_object()
            .ok_or_else(|| ToolError::Other("Git input must be an object".into()))?;
        let operation = required_string(object, "operation")?;
        let command = match operation {
            "status" => status_command(object)?,
            "diff" => diff_command(object)?,
            "log" => log_command(object)?,
            other => {
                return Err(ToolError::Other(format!(
                    "unsupported Git operation `{other}`"
                )));
            }
        };

        let workspace = object.get("workspace").and_then(JsonValue::as_str);
        let target = match ctx
            .executor
            .select_mount_target(&ctx.execution_target_for(&self.workdir), workspace)
        {
            Ok(target) => target,
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
        };
        let timeout = ctx.budget.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let output = match ctx
            .executor
            .run_read_only_command(&target, &command, timeout)
            .await
        {
            Ok(output) => output,
            Err(WorkspaceExecutorError::Timeout(timeout)) => {
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

fn status_command(object: &JsonMap<String, JsonValue>) -> Result<String, ToolError> {
    reject_unknown_fields(object, &["operation"])?;
    Ok(format!("{SAFE_GIT_PREFIX} status --short --branch"))
}

fn diff_command(object: &JsonMap<String, JsonValue>) -> Result<String, ToolError> {
    reject_unknown_fields(object, &["operation", "staged", "stat", "path"])?;
    let mut command = format!("{SAFE_GIT_PREFIX} diff --no-ext-diff --no-textconv --no-color");
    if optional_bool(object, "staged")?.unwrap_or(false) {
        command.push_str(" --cached");
    }
    if optional_bool(object, "stat")?.unwrap_or(false) {
        command.push_str(" --stat");
    }
    if let Some(path) = optional_path(object)? {
        command.push_str(" -- ");
        command.push_str(&shell_quote(path));
    }
    Ok(command)
}

fn log_command(object: &JsonMap<String, JsonValue>) -> Result<String, ToolError> {
    reject_unknown_fields(object, &["operation", "max_count", "path"])?;
    let max_count = optional_u64(object, "max_count")?.unwrap_or(DEFAULT_LOG_COUNT);
    if !(1..=MAX_LOG_COUNT).contains(&max_count) {
        return Err(ToolError::Other(format!(
            "`max_count` must be between 1 and {MAX_LOG_COUNT}"
        )));
    }
    let mut command = format!(
        "{SAFE_GIT_PREFIX} log --no-color --date=short --pretty=format:'%h %ad %s%d' -n {max_count}"
    );
    if let Some(path) = optional_path(object)? {
        command.push_str(" -- ");
        command.push_str(&shell_quote(path));
    }
    Ok(command)
}

fn required_string<'a>(
    object: &'a JsonMap<String, JsonValue>,
    field: &str,
) -> Result<&'a str, ToolError> {
    object
        .get(field)
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ToolError::Other(format!("missing required field `{field}`")))
}

fn optional_bool(
    object: &JsonMap<String, JsonValue>,
    field: &str,
) -> Result<Option<bool>, ToolError> {
    object
        .get(field)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| ToolError::Other(format!("`{field}` must be a boolean")))
        })
        .transpose()
}

fn optional_u64(
    object: &JsonMap<String, JsonValue>,
    field: &str,
) -> Result<Option<u64>, ToolError> {
    object
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| ToolError::Other(format!("`{field}` must be a positive integer")))
        })
        .transpose()
}

fn optional_path(object: &JsonMap<String, JsonValue>) -> Result<Option<&str>, ToolError> {
    let Some(value) = object.get("path") else {
        return Ok(None);
    };
    let path = value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ToolError::Other("`path` must be a non-empty string".into()))?;
    let parsed = Path::new(path);
    if path.contains('\0')
        || parsed.is_absolute()
        || parsed.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ToolError::Other(
            "`path` must stay within the current workspace".into(),
        ));
    }
    Ok(Some(path))
}

fn reject_unknown_fields(
    object: &JsonMap<String, JsonValue>,
    allowed: &[&str],
) -> Result<(), ToolError> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(ToolError::Other(format!(
            "field `{field}` is not valid for this Git operation"
        )));
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
#[path = "../../tests/unit/tools_git.rs"]
mod tests;
