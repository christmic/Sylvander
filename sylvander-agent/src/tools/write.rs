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
use serde_json::{json, Value as JsonValue};

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};

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

    async fn execute(&self, input: JsonValue) -> Result<ToolOutput, ToolError> {
        let path_str = input
            .get("file_path")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ToolError::Other("missing required field `file_path`".into()))?;
        let content = input
            .get("content")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ToolError::Other("missing required field `content`".into()))?;

        let path = self.workdir.join(path_str);

        // Create parent dirs if missing.
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Ok(ToolOutput::err(format!(
                "cannot create parent dir for `{path_str}`: {e}"
            )));
        }

        match std::fs::write(&path, content) {
            Ok(()) => Ok(ToolOutput::ok(format!(
                "wrote {} bytes to `{path_str}`",
                content.len()
            ))),
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

    fn setup_workspace() -> TempDir {
        TempDir::new().expect("tempdir")
    }

    #[tokio::test]
    async fn write_new_file() {
        let dir = setup_workspace();
        let tool = WriteTool::new(dir.path());
        let out = tool
            .execute(json!({"file_path": "out.txt", "content": "hello"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        let written = fs::read_to_string(dir.path().join("out.txt")).unwrap();
        assert_eq!(written, "hello");
    }

    #[tokio::test]
    async fn write_overwrites_existing() {
        let dir = setup_workspace();
        fs::write(dir.path().join("f.txt"), "old").unwrap();
        let tool = WriteTool::new(dir.path());
        let out = tool
            .execute(json!({"file_path": "f.txt", "content": "new"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(fs::read_to_string(dir.path().join("f.txt")).unwrap(), "new");
    }

    #[tokio::test]
    async fn write_creates_parent_dirs() {
        let dir = setup_workspace();
        let tool = WriteTool::new(dir.path());
        let out = tool
            .execute(json!({"file_path": "a/b/c/deep.txt", "content": "x"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(dir.path().join("a/b/c/deep.txt").exists());
    }

    #[tokio::test]
    async fn write_writes_empty_string() {
        let dir = setup_workspace();
        let tool = WriteTool::new(dir.path());
        let out = tool
            .execute(json!({"file_path": "empty.txt", "content": ""}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(fs::read_to_string(dir.path().join("empty.txt")).unwrap(), "");
    }

    #[tokio::test]
    async fn write_missing_file_path_field() {
        let dir = setup_workspace();
        let tool = WriteTool::new(dir.path());
        let result = tool.execute(json!({"content": "x"})).await;
        assert!(matches!(result, Err(ToolError::Other(_))));
    }

    #[tokio::test]
    async fn write_missing_content_field() {
        let dir = setup_workspace();
        let tool = WriteTool::new(dir.path());
        let result = tool.execute(json!({"file_path": "x.txt"})).await;
        assert!(matches!(result, Err(ToolError::Other(_))));
    }

    #[test]
    fn name_description_schema() {
        let dir = setup_workspace();
        let tool = WriteTool::new(dir.path());
        assert_eq!(tool.name(), "Write");
        assert!(tool.description().contains("workdir"));
        let json = serde_json::to_value(tool.input_schema()).unwrap();
        assert!(json["properties"]["file_path"].is_object());
        assert!(json["properties"]["content"].is_object());
        let required = json["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "file_path"));
        assert!(required.iter().any(|v| v == "content"));
    }

    #[test]
    fn workdir_accessor() {
        let dir = setup_workspace();
        let tool = WriteTool::new(dir.path());
        assert_eq!(tool.workdir(), dir.path());
    }
}