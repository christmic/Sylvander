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
use serde_json::{json, Value as JsonValue};

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

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

    async fn execute(
        &self,
        _ctx: &ToolContext,
        input: JsonValue,
    ) -> Result<ToolOutput, ToolError> {
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

        let path = self.workdir.join(path_str);

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput::err(format!(
                    "cannot read `{path_str}`: {e}"
                )));
            }
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

        match std::fs::write(&path, &new_content) {
            Ok(()) => {
                let replaced = if replace_all { occurrences } else { 1 };
                Ok(ToolOutput::ok(format!(
                    "replaced {replaced} occurrence{} in `{}`",
                    if replaced == 1 { "" } else { "s" },
                    path_str
                )))
            }
            Err(e) => Ok(ToolOutput::err(format!(
                "cannot write `{path_str}`: {e}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    use crate::tool_context::ToolContext;
    fn ctx() -> ToolContext { ToolContext::new("u", "a", "s") }

    fn setup_workspace() -> TempDir {
        TempDir::new().expect("tempdir")
    }

    #[tokio::test]
    async fn edit_unique_match() {
        let dir = setup_workspace();
        fs::write(dir.path().join("f.txt"), "hello world").unwrap();
        let tool = EditTool::new(dir.path());
        let c = ctx();
        let out = tool
            .execute(&c, json!({
                "file_path": "f.txt",
                "old_string": "world",
                "new_string": "rust"
            }))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(fs::read_to_string(dir.path().join("f.txt")).unwrap(), "hello rust");
    }

    #[tokio::test]
    async fn edit_multiple_occurrences_errors_by_default() {
        let dir = setup_workspace();
        fs::write(dir.path().join("f.txt"), "aaa aaa aaa").unwrap();
        let tool = EditTool::new(dir.path());
        let c = ctx();
        let out = tool
            .execute(&c, json!({
                "file_path": "f.txt",
                "old_string": "aaa",
                "new_string": "bbb"
            }))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("appears 3 times"));
        // File unchanged
        assert_eq!(fs::read_to_string(dir.path().join("f.txt")).unwrap(), "aaa aaa aaa");
    }

    #[tokio::test]
    async fn edit_replace_all() {
        let dir = setup_workspace();
        fs::write(dir.path().join("f.txt"), "aaa aaa aaa").unwrap();
        let tool = EditTool::new(dir.path());
        let c = ctx();
        let out = tool
            .execute(&c, json!({
                "file_path": "f.txt",
                "old_string": "aaa",
                "new_string": "bbb",
                "replace_all": true
            }))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(fs::read_to_string(dir.path().join("f.txt")).unwrap(), "bbb bbb bbb");
    }

    #[tokio::test]
    async fn edit_old_string_not_found() {
        let dir = setup_workspace();
        fs::write(dir.path().join("f.txt"), "hello").unwrap();
        let tool = EditTool::new(dir.path());
        let c = ctx();
        let out = tool
            .execute(&c, json!({
                "file_path": "f.txt",
                "old_string": "missing",
                "new_string": "x"
            }))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("not found"));
    }

    #[tokio::test]
    async fn edit_identical_strings() {
        let dir = setup_workspace();
        fs::write(dir.path().join("f.txt"), "hello").unwrap();
        let tool = EditTool::new(dir.path());
        let c = ctx();
        let out = tool
            .execute(&c, json!({
                "file_path": "f.txt",
                "old_string": "hello",
                "new_string": "hello"
            }))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("identical"));
    }

    #[tokio::test]
    async fn edit_missing_file_path_field() {
        let dir = setup_workspace();
        let tool = EditTool::new(dir.path());
        let c = ctx();
        let result = tool
            .execute(&c, json!({"old_string": "a", "new_string": "b"}))
            .await;
        assert!(matches!(result, Err(ToolError::Other(_))));
    }

    #[tokio::test]
    async fn edit_missing_old_string_field() {
        let dir = setup_workspace();
        let tool = EditTool::new(dir.path());
        let c = ctx();
        let result = tool
            .execute(&c, json!({"file_path": "f.txt", "new_string": "b"}))
            .await;
        assert!(matches!(result, Err(ToolError::Other(_))));
    }

    #[tokio::test]
    async fn edit_missing_new_string_field() {
        let dir = setup_workspace();
        let tool = EditTool::new(dir.path());
        let c = ctx();
        let result = tool
            .execute(&c, json!({"file_path": "f.txt", "old_string": "a"}))
            .await;
        assert!(matches!(result, Err(ToolError::Other(_))));
    }

    #[tokio::test]
    async fn edit_multiline_old_string() {
        let dir = setup_workspace();
        let original = "line1\nline2\nline3\n";
        fs::write(dir.path().join("f.txt"), original).unwrap();
        let tool = EditTool::new(dir.path());
        let c = ctx();
        let out = tool
            .execute(&c, json!({
                "file_path": "f.txt",
                "old_string": "line2\nline3",
                "new_string": "REPLACED"
            }))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(
            fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "line1\nREPLACED\n"
        );
    }

    #[test]
    fn name_description_schema() {
        let dir = setup_workspace();
        let tool = EditTool::new(dir.path());
        let c = ctx();
        assert_eq!(tool.name(), "Edit");
        assert!(tool.description().contains("replace"));
        let json = serde_json::to_value(tool.input_schema()).unwrap();
        assert!(json["properties"]["old_string"].is_object());
        assert!(json["properties"]["new_string"].is_object());
        assert!(json["properties"]["replace_all"].is_object());
    }
}