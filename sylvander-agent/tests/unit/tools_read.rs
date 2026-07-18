use super::*;
use crate::tool_context::ToolContext;
use std::fs;
use tempfile::TempDir;

fn ctx() -> ToolContext {
    ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
        .with_capability(crate::tool_context::Cap::Read)
        .with_capability(crate::tool_context::Cap::Write)
        .with_capability(crate::tool_context::Cap::MemoryRead)
        .with_capability(crate::tool_context::Cap::MemoryWrite)
}

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
fn progress_chunks_preserve_unicode_without_empty_tail() {
    assert_eq!(output_chunks("蟹🦀abc", 2), ["蟹🦀", "ab", "c"]);
    assert!(output_chunks("", 2).is_empty());
}

#[test]
fn read_existing_file() {
    let (_dir, workdir) = setup_workspace();
    let tool = ReadTool::new(&workdir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let c = ctx();
    let out = rt
        .block_on(tool.execute(&c, json!({"file_path": "hello.txt"})))
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(out.content, "Hello, world!");
}

#[test]
fn read_nested_file() {
    let (_dir, workdir) = setup_workspace();
    let tool = ReadTool::new(&workdir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let c = ctx();
    let out = rt
        .block_on(tool.execute(&c, json!({"file_path": "sub/nested.txt"})))
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(out.content, "nested content");
}

#[test]
fn read_empty_file() {
    let (_dir, workdir) = setup_workspace();
    let tool = ReadTool::new(&workdir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let c = ctx();
    let out = rt
        .block_on(tool.execute(&c, json!({"file_path": "empty.txt"})))
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(out.content, "");
}

#[tokio::test]
async fn read_missing_file_returns_err() {
    let (_dir, workdir) = setup_workspace();
    let tool = ReadTool::new(&workdir);
    let c = ctx();
    let out = tool
        .execute(&c, json!({"file_path": "does_not_exist.txt"}))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("cannot resolve"));
}

#[tokio::test]
async fn read_rejects_oversized_file_without_returning_partial_content() {
    let (_dir, workdir) = setup_workspace();
    fs::write(
        workdir.join("large.txt"),
        vec![b'x'; MAX_READ_FILE_BYTES + 1],
    )
    .unwrap();

    let out = ReadTool::new(&workdir)
        .execute(&ctx(), json!({"file_path": "large.txt"}))
        .await
        .unwrap();

    assert!(out.is_error);
    assert!(out.content.contains("file too large"));
    assert!(out.content.contains(&(MAX_READ_FILE_BYTES + 1).to_string()));
}

#[tokio::test]
async fn read_missing_file_path_field() {
    let (_dir, workdir) = setup_workspace();
    let tool = ReadTool::new(&workdir);
    let c = ctx();
    let result = tool.execute(&c, json!({})).await;
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
    let c = ctx();
    let result = tool
        .execute(&c, json!({"file_path": "../etc/passwd"}))
        .await;
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
    let c = ctx();
    let result = tool.execute(&c, json!({"file_path": "escape.txt"})).await;

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
