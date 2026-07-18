//! `Edit` tool — in-place text replacement in a file.
//!
//! Replaces `old_string` with `new_string` in a file. By default
//! requires the match to be unique (so the LLM can't accidentally
//! replace a substring that appears multiple times). Set
//! `replace_all: true` to allow replacing all occurrences.
//!
//! Failures are returned as `ToolOutput::err` so the model can
//! react (e.g., "string not found, please re-read the file").

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

const MAX_EDIT_FILE_BYTES: usize = 8 * 1024 * 1024;

/// Replace text in a file. Paths are resolved relative to `workdir`.
#[derive(Debug, Clone)]
pub struct EditTool {
    workdir: PathBuf,
}

impl EditTool {
    /// Create an `EditTool` rooted at `workdir`.
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
impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "Edit"
    }

    fn description(&self) -> &'static str {
        "Replace a string in a file with a new string. By default, \
         the old_string must appear exactly once in the file (so the \
         replacement is unambiguous). Set replace_all to true to \
         replace every occurrence. Paths are relative to the workdir."
    }

    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            json!({
                "file_path": {
                    "type": "string",
                    "description": "Path to the file, relative to the workdir"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to find and replace (must match exactly)"
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace it with"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence instead of requiring a unique match (default false)"
                }
            }),
            &["file_path", "old_string", "new_string"],
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
        let old_string = input
            .get("old_string")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ToolError::Other("missing required field `old_string`".into()))?;
        let new_string = input
            .get("new_string")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ToolError::Other("missing required field `new_string`".into()))?;
        let replace_all = input
            .get("replace_all")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);

        if old_string == new_string {
            return Ok(ToolOutput::err(String::from(
                "old_string and new_string are identical; nothing to do",
            )));
        }

        let target = ctx.execution_target_for(&self.workdir);
        if target.read_only {
            return Ok(ToolOutput::err(format!(
                "execution target `{}` is read-only",
                target.id
            )));
        }
        let read = match ctx
            .executor
            .read_file_bounded(&target, path_str, MAX_EDIT_FILE_BYTES)
            .await
        {
            Ok(read) => read,
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
        };
        if read.truncated {
            return Ok(ToolOutput::err(format!(
                "file too large to edit ({} bytes > {} byte limit)",
                read.total_bytes, MAX_EDIT_FILE_BYTES
            )));
        }
        let content = match String::from_utf8(read.bytes) {
            Ok(content) => content,
            Err(error) => return Ok(ToolOutput::err(format!("file is not UTF-8 text: {error}"))),
        };

        let occurrences = content.matches(old_string).count();
        if occurrences == 0 {
            return Ok(ToolOutput::err(format!(
                "old_string not found in `{path_str}`"
            )));
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            if occurrences > 1 {
                return Ok(ToolOutput::err(format!(
                    "old_string appears {occurrences} times in `{path_str}`; \
                     either make it unique or set replace_all=true"
                )));
            }
            content.replacen(old_string, new_string, 1)
        };

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
                        new_content.as_bytes(),
                    )
                    .map_err(ToolError::Other)?,
            )
        } else {
            None
        };

        match ctx
            .executor
            .write_file(&target, path_str, new_content.as_bytes())
            .await
        {
            Ok(()) => {
                if let (Some(journal), Some(prepared)) = (&ctx.workspace_journal, &prepared) {
                    journal.commit(prepared).map_err(ToolError::Other)?;
                }
                let replaced = if replace_all { occurrences } else { 1 };
                Ok(ToolOutput::ok(format!(
                    "replaced {replaced} occurrence{} in `{}`",
                    if replaced == 1 { "" } else { "s" },
                    path_str
                )))
            }
            Err(error) => Ok(ToolOutput::err(error.to_string())),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/tools_edit.rs"]
mod tests;
