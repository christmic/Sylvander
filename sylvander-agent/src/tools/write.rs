//! `Write` tool — write a file to disk.
//!
//! Basic implementation: takes a path (relative to a workdir) and
//! content, writes the content to the file. Creates parent
//! directories if needed. Overwrites existing files.
//!
//! Failures (parent dir not creatable, permission denied, etc.) are
//! returned as `ToolOutput::err` so the model can react.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

/// Write a file to disk. Paths are resolved relative to `workdir`.
/// If the parent directory does not exist, it is created.
#[derive(Debug, Clone)]
pub struct WriteTool {
    workdir: PathBuf,
}

impl WriteTool {
    /// Create a `WriteTool` rooted at `workdir`.
    #[must_use]
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }

    /// Current working directory.
    #[must_use]
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "Write"
    }

    fn description(&self) -> &'static str {
        "Write content to a file at the given path (relative to workdir). \
         Creates parent directories if needed. Overwrites the file if it already exists."
    }

    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            json!({
                "file_path": {
                    "type": "string",
                    "description": "Path to the file, relative to the workdir"
                },
                "content": {
                    "type": "string",
                    "description": "The full file content to write"
                }
            }),
            &["file_path", "content"],
        )
    }

    fn invocation_class(&self) -> crate::tool_invocation::ToolInvocationClass {
        crate::tool_invocation::ToolInvocationClass::FilesystemMutation
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(crate::tool_context::Cap::Write) {
            return Ok(ToolOutput::err(
                "write capability not granted for this invocation",
            ));
        }
        let path_str = input
            .get("file_path")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ToolError::Other("missing required field `file_path`".into()))?;
        let content = input
            .get("content")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ToolError::Other("missing required field `content`".into()))?;

        let target = ctx.execution_target_for(&self.workdir);
        if target.read_only {
            return Ok(ToolOutput::err(format!(
                "execution target `{}` is read-only",
                target.id
            )));
        }
        let prepared = if let Some(journal) = &ctx.workspace_journal {
            let turn_id = ctx.session.request.trace_id.as_deref().ok_or_else(|| {
                ToolError::Other("workspace journal requires a turn trace id".into())
            })?;
            Some(
                journal
                    .prepare(
                        &ctx.session_id().0,
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
            .write_file(&target, path_str, content.as_bytes())
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
