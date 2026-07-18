use super::*;
use std::fs;
use tempfile::TempDir;

use crate::tool_context::ToolContext;
fn ctx() -> ToolContext {
    ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
        .with_capability(crate::tool_context::Cap::Read)
        .with_capability(crate::tool_context::Cap::Write)
        .with_capability(crate::tool_context::Cap::MemoryRead)
        .with_capability(crate::tool_context::Cap::MemoryWrite)
}

fn setup_workspace() -> TempDir {
    TempDir::new().expect("tempdir")
}

#[tokio::test]
async fn write_new_file() {
    let dir = setup_workspace();
    let tool = WriteTool::new(dir.path());
    let c = ctx();
    let out = tool
        .execute(&c, json!({"file_path": "out.txt", "content": "hello"}))
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
    let c = ctx();
    let out = tool
        .execute(&c, json!({"file_path": "f.txt", "content": "new"}))
        .await
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(fs::read_to_string(dir.path().join("f.txt")).unwrap(), "new");
}

#[tokio::test]
async fn write_creates_parent_dirs() {
    let dir = setup_workspace();
    let tool = WriteTool::new(dir.path());
    let c = ctx();
    let out = tool
        .execute(&c, json!({"file_path": "a/b/c/deep.txt", "content": "x"}))
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(dir.path().join("a/b/c/deep.txt").exists());
}

#[tokio::test]
async fn write_writes_empty_string() {
    let dir = setup_workspace();
    let tool = WriteTool::new(dir.path());
    let c = ctx();
    let out = tool
        .execute(&c, json!({"file_path": "empty.txt", "content": ""}))
        .await
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(
        fs::read_to_string(dir.path().join("empty.txt")).unwrap(),
        ""
    );
}

#[tokio::test]
async fn write_missing_file_path_field() {
    let dir = setup_workspace();
    let tool = WriteTool::new(dir.path());
    let _c = ctx();
    let c = ctx();
    let result = tool.execute(&c, json!({"content": "x"})).await;
    assert!(matches!(result, Err(ToolError::Other(_))));
}

#[tokio::test]
async fn write_missing_content_field() {
    let dir = setup_workspace();
    let tool = WriteTool::new(dir.path());
    let _c = ctx();
    let c = ctx();
    let result = tool.execute(&c, json!({"file_path": "x.txt"})).await;
    assert!(matches!(result, Err(ToolError::Other(_))));
}

#[test]
fn name_description_schema() {
    let dir = setup_workspace();
    let tool = WriteTool::new(dir.path());
    let _c = ctx();
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
    let _c = ctx();
    assert_eq!(tool.workdir(), dir.path());
}
