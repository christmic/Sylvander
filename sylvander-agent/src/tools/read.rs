//! `Read` tool — read a file from disk and return its contents.
//!
//! The canonical first tool in any agent framework. Safe (no side
//! effects), universally useful, and exercises the full loop flow:
//! `tool_use → execute → tool_result → re-feed → next iteration`.
//!
//! # Path safety
//!
//! Paths are resolved relative to the configured `workdir`. Symlink
//! traversal outside `workdir` is blocked by checking the
//! canonicalized path. The `ToolError::Other` variant is used for
//! all filesystem failures — they terminate the loop with the error
//! surfaced to the caller.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value as JsonValue};

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};

/// Read a file from disk. Paths are resolved relative to `workdir`.
#[derive(Debug, Clone)]
pub struct ReadTool {
    workdir: PathBuf,
}

impl ReadTool {
    /// Create a `ReadTool` rooted at `workdir`. Files outside this
    /// directory (after symlink resolution) are rejected.
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
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "Read"
    }

    fn description(&self) -> &'static str {
        "Read the contents of a file at the given path (relative to workdir). \
         Returns the file's text content. Rejects paths that escape the workdir."
    }

    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            json!({
                "file_path": {
                    "type": "string",
                    "description": "Path to the file, relative to the workdir"
                }
            }),
            &["file_path"],
        )
    }

    async fn execute(&self, input: JsonValue) -> Result<ToolOutput, ToolError> {
        let path_str = input
            .get("file_path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Other("missing required field `file_path`".into()))?;

        // Canonicalize the workdir first so that symlink-resolved
        // paths (e.g., /var/folders/... → /private/var/folders/... on
        // macOS) are compared on equal footing with the requested path.
        let workdir_canonical = match self.workdir.canonicalize() {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::err(format!("cannot canonicalize workdir: {e}"))),
        };

        // Resolve the requested path against workdir
        let requested = self.workdir.join(path_str);
        let canonical = match requested.canonicalize() {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::err(format!("cannot resolve `{path_str}`: {e}"))),
        };

        // Reject path traversal (e.g., "../etc/passwd" or symlinks
        // pointing outside workdir) — security violation, terminate.
        if !canonical.starts_with(&workdir_canonical) {
            return Err(ToolError::Other(format!(
                "path `{path_str}` escapes workdir"
            )));
        }

        // Read the file. Cap at 1 MiB to avoid runaway memory.
        let content = match std::fs::read_to_string(&canonical) {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput::err(format!(
                    "cannot read `{}`: {e}",
                    canonical.display()
                )));
            }
        };

        const MAX_BYTES: usize = 1024 * 1024;
        if content.len() > MAX_BYTES {
            return Ok(ToolOutput::err(format!(
                "file too large ({} bytes > {} byte limit)",
                content.len(),
                MAX_BYTES
            )));
        }

        Ok(ToolOutput::ok(content))
    }
}

// Bring `as_str` into scope as a method on `serde_json::Value` (the
// `Value` alias is in the prelude; the method comes from the trait).
use serde_json::Value;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create a temp dir with a few files.
    fn setup_workspace() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let workdir = dir.path().to_path_buf();
        fs::write(workdir.join("hello.txt"), "Hello, world!").unwrap();
        fs::write(workdir.join("empty.txt"), "").unwrap();
        fs::create_dir(workdir.join("sub")).unwrap();
        fs::write(workdir.join("sub/nested.txt"), "nested content").unwrap();
        (dir, workdir)
    }

    #[test]
    fn read_existing_file() {
        let (_dir, workdir) = setup_workspace();
        let tool = ReadTool::new(&workdir);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt.block_on(tool.execute(json!({"file_path": "hello.txt"}))).unwrap();
        assert!(!out.is_error);
        assert_eq!(out.content, "Hello, world!");
    }

    #[test]
    fn read_nested_file() {
        let (_dir, workdir) = setup_workspace();
        let tool = ReadTool::new(&workdir);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt
            .block_on(tool.execute(json!({"file_path": "sub/nested.txt"})))
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.content, "nested content");
    }

    #[test]
    fn read_empty_file() {
        let (_dir, workdir) = setup_workspace();
        let tool = ReadTool::new(&workdir);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt
            .block_on(tool.execute(json!({"file_path": "empty.txt"})))
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.content, "");
    }

    #[tokio::test]
    async fn read_missing_file_returns_err() {
        let (_dir, workdir) = setup_workspace();
        let tool = ReadTool::new(&workdir);
        let out = tool
            .execute(json!({"file_path": "does_not_exist.txt"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("cannot resolve"));
    }

    #[tokio::test]
    async fn read_missing_file_path_field() {
        let (_dir, workdir) = setup_workspace();
        let tool = ReadTool::new(&workdir);
        let result = tool.execute(json!({})).await;
        assert!(matches!(result, Err(ToolError::Other(_))));
    }

    #[tokio::test]
    async fn read_path_outside_workdir_rejected() {
        let (_dir, workdir) = setup_workspace();
        let tool = ReadTool::new(&workdir);
        // Try a path that resolves outside workdir. On most CI,
        // the parent dir exists but the requested file doesn't —
        // the canonicalize fails first with "No such file", which
        // we surface as a model-visible error. To exercise the actual
        // traversal check, we create a real symlink in setup_workspace
        // (next test).
        let result = tool.execute(json!({"file_path": "../etc/passwd"})).await;
        // Either Err (security violation) or Ok(ToolOutput::err(...)) (file
        // not found) — both are correct rejections. The point is the
        // file content is NOT returned.
        if let Ok(out) = result {
            assert!(out.is_error);
        }
    }

    #[tokio::test]
    async fn read_path_via_symlink_outside_workdir_rejected() {
        use std::os::unix::fs::symlink;
        let (dir, workdir) = setup_workspace();
        // Create a symlink inside workdir that points outside it
        let outside_file = dir.path().parent().unwrap().join("outside.txt");
        std::fs::write(&outside_file, "SECRET").unwrap();
        symlink(&outside_file, workdir.join("escape.txt")).unwrap();

        let tool = ReadTool::new(&workdir);
        let result = tool.execute(json!({"file_path": "escape.txt"})).await;

        // Traversal is a security violation, NOT a model-visible
        // error — must surface as `Err(ToolError::Other)` so the
        // AgentLoop terminates rather than asking the model to react.
        match result {
            Err(ToolError::Other(msg)) => {
                assert!(
                    msg.contains("escapes workdir"),
                    "expected 'escapes workdir' in error, got: {msg}"
                );
            }
            other => panic!("expected Err(ToolError::Other) for traversal, got {other:?}"),
        }
    }

    #[test]
    fn name_description_schema() {
        let (_dir, workdir) = setup_workspace();
        let tool = ReadTool::new(&workdir);
        assert_eq!(tool.name(), "Read");
        assert!(tool.description().contains("workdir"));
        let schema = tool.input_schema();
        // schema is the flattened JSON object, must contain file_path
        let json = serde_json::to_value(&schema).unwrap();
        assert!(json["properties"]["file_path"].is_object());
        assert_eq!(json["required"][0], "file_path");
    }

    #[test]
    fn workdir_accessor() {
        let (_dir, workdir) = setup_workspace();
        let tool = ReadTool::new(&workdir);
        assert_eq!(tool.workdir(), workdir.as_path());
    }
}