//! Structured workspace directory listing.

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};
use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::{Cap, ToolContext};
use crate::workspace_executor::{
    MAX_QUERY_RESULTS, WorkspaceEntryKind, WorkspaceListRequest, WorkspaceQueryLimits,
};

/// List files and directories through the invocation's workspace executor.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListTool;

impl ListTool {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ListTool {
    fn name(&self) -> &'static str {
        "List"
    }

    fn description(&self) -> &'static str {
        "List files and directories in the current workspace without invoking a shell. Returns compact JSON with path, kind, size, and an explicit truncated flag."
    }

    fn input_schema(&self) -> InputSchema {
        InputSchema::from_json_value(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path relative to the workspace (default: .)"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Whether to recursively list descendants (default: false)"
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_QUERY_RESULTS,
                    "description": "Maximum entries to return (default: 200, hard limit: 1000)"
                }
            },
            "additionalProperties": false
        }))
    }

    fn invocation_class(&self) -> crate::tool_invocation::ToolInvocationClass {
        crate::tool_invocation::ToolInvocationClass::Read
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(Cap::Read) {
            return Ok(ToolOutput::err(
                "list capability not granted for this invocation",
            ));
        }
        let object = strict_object(&input, &["path", "recursive", "max_results"])?;
        let path = optional_string(object.get("path"), "path")?.unwrap_or(".");
        let recursive = optional_bool(object.get("recursive"), "recursive")?.unwrap_or(false);
        let max_results = parse_max_results(object.get("max_results"))?;
        let limits = WorkspaceQueryLimits {
            max_results,
            ..WorkspaceQueryLimits::default()
        };
        let target = match ctx.require_execution_target() {
            Ok(target) => target,
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
        };
        let result = match ctx
            .executor
            .list(
                target,
                WorkspaceListRequest {
                    relative_path: path.into(),
                    recursive,
                    limits,
                },
            )
            .await
        {
            Ok(result) => result,
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
        };
        let entries = result
            .entries
            .into_iter()
            .map(|entry| {
                json!({
                    "path": entry.relative_path,
                    "kind": kind_name(entry.kind),
                    "size": entry.size,
                })
            })
            .collect::<Vec<_>>();
        Ok(ToolOutput::ok(
            serde_json::to_string(&json!({
                "entries": entries,
                "truncated": result.truncated,
            }))
            .expect("workspace list result is serializable"),
        ))
    }
}

fn kind_name(kind: WorkspaceEntryKind) -> &'static str {
    match kind {
        WorkspaceEntryKind::File => "file",
        WorkspaceEntryKind::Directory => "directory",
        WorkspaceEntryKind::Symlink => "symlink",
        WorkspaceEntryKind::Other => "other",
    }
}

pub(super) fn strict_object<'a>(
    input: &'a JsonValue,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, JsonValue>, ToolError> {
    let object = input
        .as_object()
        .ok_or_else(|| ToolError::Other("tool input must be an object".into()))?;
    if let Some(field) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ToolError::Other(format!("unknown input field `{field}`")));
    }
    Ok(object)
}

pub(super) fn optional_string<'a>(
    value: Option<&'a JsonValue>,
    field: &str,
) -> Result<Option<&'a str>, ToolError> {
    value
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| ToolError::Other(format!("`{field}` must be a string")))
        })
        .transpose()
}

fn optional_bool(value: Option<&JsonValue>, field: &str) -> Result<Option<bool>, ToolError> {
    value
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| ToolError::Other(format!("`{field}` must be a boolean")))
        })
        .transpose()
}

pub(super) fn parse_max_results(value: Option<&JsonValue>) -> Result<usize, ToolError> {
    let Some(value) = value else {
        return Ok(WorkspaceQueryLimits::default().max_results);
    };
    let value = value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (1..=MAX_QUERY_RESULTS).contains(value))
        .ok_or_else(|| {
            ToolError::Other(format!(
                "`max_results` must be an integer between 1 and {MAX_QUERY_RESULTS}"
            ))
        })?;
    Ok(value)
}

#[cfg(test)]
#[path = "../../tests/unit/tools_list.rs"]
mod tests;
