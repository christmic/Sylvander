//! `Write` tool — write a file to disk.
//!
//! Basic implementation: takes a path (relative to the current workspace) and
//! content, writes the content to the file. Creates parent
//! directories if needed. Overwrites existing files.
//!
//! Failures (parent dir not creatable, permission denied, etc.) are
//! returned as `ToolOutput::err` so the model can react.

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};

use sylvander_llm_core::InputSchema;

#[cfg(test)]
use crate::tool::ToolTestExt as _;
use crate::tool::{
    PreparedToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput, ToolSpec,
};
use crate::tool_context::ToolContext;

/// Write a file into the invocation's explicit workspace.
/// If the parent directory does not exist, it is created.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteTool;

impl WriteTool {
    /// Create a stateless write tool.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ToolDefinition for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::strict(
            "Write",
            "Write content to a file at the given path (relative to the current workspace). Creates parent directories if needed. Overwrites the file if it already exists.",
            InputSchema::new_with_properties(
                json!({
                    "file_path": {
                        "type": "string",
                        "description": "Path to the file, relative to the current workspace"
                    },
                    "content": {
                        "type": "string",
                        "description": "The full file content to write"
                    }
                }),
                &["file_path", "content"],
            )
            .schema,
            crate::tool_invocation::ToolInvocationClass::FilesystemMutation,
        )
    }
}

#[async_trait]
impl ToolExecutor for WriteTool {
    async fn handle(
        &self,
        ctx: &ToolContext,
        call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(crate::tool_context::Cap::Write) {
            return Ok(ToolOutput::err(
                "write capability not granted for this invocation",
            ));
        }
        let path_str = call
            .input()
            .get("file_path")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ToolError::Other("missing required field `file_path`".into()))?;
        let content = call
            .input()
            .get("content")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ToolError::Other("missing required field `content`".into()))?;

        let target = match ctx.require_execution_target() {
            Ok(target) => target,
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
        };
        if target.read_only {
            return Ok(ToolOutput::err(format!(
                "execution target `{}` is read-only",
                target.id
            )));
        }
        let prepared = if let Some(journal) = &ctx.workspace_journal {
            let turn_id = ctx.trace_id().ok_or_else(|| {
                ToolError::Other("workspace journal requires a turn trace id".into())
            })?;
            Some(
                journal
                    .prepare(
                        ctx.session_id(),
                        turn_id,
                        &target.workspace_path,
                        path_str,
                        content.as_bytes(),
                    )
                    .map_err(ToolError::Other)?,
            )
        } else {
            None
        };

        match ctx
            .executor
            .write_file(target, path_str, content.as_bytes())
            .await
        {
            Ok(()) => {
                if let (Some(journal), Some(prepared)) = (&ctx.workspace_journal, &prepared) {
                    journal.commit(prepared).map_err(ToolError::Other)?;
                }
                Ok(ToolOutput::ok(format!(
                    "wrote {} bytes to `{path_str}`",
                    content.len()
                )))
            }
            Err(error) => Ok(ToolOutput::err(error.to_string())),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/tools_write.rs"]
mod tests;
