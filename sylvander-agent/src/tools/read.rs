//! `Read` tool — read a file from disk and return its contents.
//!
//! The canonical first tool in any agent framework. Safe (no side
//! effects), universally useful, and exercises the full loop flow:
//! `tool_use → execute → tool_result → re-feed → next iteration`.
//!
//! # Path safety
//!
//! Paths are resolved relative to the invocation's explicit workspace.
//! Symlink traversal outside that workspace is blocked by checking the
//! canonicalized path. The `ToolError::Other` variant is used for
//! all filesystem failures — they terminate the loop with the error
//! surfaced to the caller.

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput, ToolProgressSink};
use crate::tool_context::ToolContext;

const MAX_READ_FILE_BYTES: usize = 1024 * 1024;

/// Read a file from the invocation's explicit workspace.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadTool;

impl ReadTool {
    /// Create a stateless read tool.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "Read"
    }

    fn description(&self) -> &'static str {
        "Read the contents of a file at the given path (relative to the current workspace). \
         Returns the file's text content. Rejects paths that escape the workspace."
    }

    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            json!({
                "file_path": {
                    "type": "string",
                    "description": "Path to the file, relative to the current workspace"
                }
            }),
            &["file_path"],
        )
    }

    fn invocation_class(&self) -> crate::tool_invocation::ToolInvocationClass {
        crate::tool_invocation::ToolInvocationClass::Read
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(crate::tool_context::Cap::Read) {
            return Ok(ToolOutput::err(
                "read capability not granted for this invocation",
            ));
        }

        let path_str = input
            .get("file_path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Other("missing required field `file_path`".into()))?;

        let target = match ctx.require_execution_target() {
            Ok(target) => target,
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
        };
        let read = match ctx
            .executor
            .read_file_bounded(target, path_str, MAX_READ_FILE_BYTES)
            .await
        {
            Ok(read) => read,
            Err(crate::workspace_executor::WorkspaceExecutorError::InvalidPath(_)) => {
                return Err(ToolError::Other(format!(
                    "path `{path_str}` escapes workspace"
                )));
            }
            Err(crate::workspace_executor::WorkspaceExecutorError::Io(error)) => {
                return Ok(ToolOutput::err(format!(
                    "cannot resolve `{path_str}`: {error}"
                )));
            }
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
        };

        if read.truncated {
            return Ok(ToolOutput::err(format!(
                "file too large ({} bytes > {} byte limit)",
                read.total_bytes, MAX_READ_FILE_BYTES
            )));
        }
        let content = match String::from_utf8(read.bytes) {
            Ok(content) => content,
            Err(error) => return Ok(ToolOutput::err(format!("file is not UTF-8 text: {error}"))),
        };

        Ok(ToolOutput::ok(content))
    }

    async fn execute_streaming(
        &self,
        ctx: &ToolContext,
        input: JsonValue,
        progress: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError> {
        let output = self.execute(ctx, input).await?;
        if !output.is_error {
            for delta in output_chunks(&output.content, 4096) {
                progress.emit(delta);
                tokio::task::yield_now().await;
            }
        }
        Ok(output)
    }
}

fn output_chunks(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() || max_chars == 0 {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0;
    for character in text.chars() {
        current.push(character);
        current_chars += 1;
        if current_chars == max_chars {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

// Bring `as_str` into scope as a method on `serde_json::Value` (the
// `Value` alias is in the prelude; the method comes from the trait).
use serde_json::Value;

#[cfg(test)]
#[path = "../../tests/unit/tools_read.rs"]
mod tests;
