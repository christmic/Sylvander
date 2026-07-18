use super::*;
use std::fs;
use tempfile::TempDir;

use crate::tool_context::ToolContext;
fn ctx(root: &std::path::Path) -> ToolContext {
    ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
        .with_fs_root(root)
        .with_capability(crate::tool_context::Cap::Read)
        .with_capability(crate::tool_context::Cap::Write)
        .with_capability(crate::tool_context::Cap::MemoryRead)
        .with_capability(crate::tool_context::Cap::MemoryWrite)
}

fn setup_workspace() -> TempDir {
    TempDir::new().expect("tempdir")
}

#[tokio::test]
async fn edit_unique_match() {
    let dir = setup_workspace();
    fs::write(dir.path().join("f.txt"), "hello world").unwrap();
    let tool = EditTool::new();
    let c = ctx(dir.path());
    let out = tool
        .execute(
            &c,
            json!({
                "file_path": "f.txt",
                "old_string": "world",
                "new_string": "rust"
            }),
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(
        fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "hello rust"
    );
}

#[tokio::test]
async fn edit_is_recorded_by_the_workspace_journal() {
    let dir = setup_workspace();
    let data = TempDir::new().unwrap();
    let file = dir.path().join("f.txt");
    fs::write(&file, "hello world").unwrap();
    let journal = std::sync::Arc::new(crate::workspace_journal::WorkspaceJournal::new(data.path()));
    let context = ToolContext::new(
        sylvander_protocol::SessionContext::new("u", "a", "s").with_trace_id("turn-1"),
    )
    .with_fs_root(dir.path())
    .with_capability(crate::tool_context::Cap::Write)
    .with_workspace_journal(journal.clone());
    EditTool::new()
        .execute(
            &context,
            json!({"file_path":"f.txt","old_string":"world","new_string":"agent"}),
        )
        .await
        .unwrap();
    let preview = journal.preview_latest_turn("s").unwrap();
    journal.rollback_latest_turn("s", &preview.turn_id).unwrap();
    assert_eq!(fs::read_to_string(file).unwrap(), "hello world");
}

#[tokio::test]
async fn edit_multiple_occurrences_errors_by_default() {
    let dir = setup_workspace();
    fs::write(dir.path().join("f.txt"), "aaa aaa aaa").unwrap();
    let tool = EditTool::new();
    let c = ctx(dir.path());
    let out = tool
        .execute(
            &c,
            json!({
                "file_path": "f.txt",
                "old_string": "aaa",
                "new_string": "bbb"
            }),
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("appears 3 times"));
    // File unchanged
    assert_eq!(
        fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "aaa aaa aaa"
    );
}

#[tokio::test]
async fn edit_replace_all() {
    let dir = setup_workspace();
    fs::write(dir.path().join("f.txt"), "aaa aaa aaa").unwrap();
    let tool = EditTool::new();
    let c = ctx(dir.path());
    let out = tool
        .execute(
            &c,
            json!({
                "file_path": "f.txt",
                "old_string": "aaa",
                "new_string": "bbb",
                "replace_all": true
            }),
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(
        fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "bbb bbb bbb"
    );
}

#[tokio::test]
async fn edit_old_string_not_found() {
    let dir = setup_workspace();
    fs::write(dir.path().join("f.txt"), "hello").unwrap();
    let tool = EditTool::new();
    let c = ctx(dir.path());
    let out = tool
        .execute(
            &c,
            json!({
                "file_path": "f.txt",
                "old_string": "missing",
                "new_string": "x"
            }),
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("not found"));
}

#[tokio::test]
async fn edit_rejects_oversized_file_without_modifying_it() {
    let dir = setup_workspace();
    let path = dir.path().join("large.txt");
    let file = fs::File::create(&path).unwrap();
    file.set_len((MAX_EDIT_FILE_BYTES + 1) as u64).unwrap();

    let out = EditTool::new()
        .execute(
            &ctx(dir.path()),
            json!({
                "file_path": "large.txt",
                "old_string": "a",
                "new_string": "b"
            }),
        )
        .await
        .unwrap();

    assert!(out.is_error);
    assert!(out.content.contains("file too large to edit"));
    assert_eq!(
        fs::metadata(path).unwrap().len(),
        (MAX_EDIT_FILE_BYTES + 1) as u64
    );
}

#[tokio::test]
async fn edit_identical_strings() {
    let dir = setup_workspace();
    fs::write(dir.path().join("f.txt"), "hello").unwrap();
    let tool = EditTool::new();
    let c = ctx(dir.path());
    let out = tool
        .execute(
            &c,
            json!({
                "file_path": "f.txt",
                "old_string": "hello",
                "new_string": "hello"
            }),
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("identical"));
}

#[tokio::test]
async fn edit_missing_file_path_field() {
    let dir = setup_workspace();
    let tool = EditTool::new();
    let c = ctx(dir.path());
    let result = tool
        .execute(&c, json!({"old_string": "a", "new_string": "b"}))
        .await;
    assert!(matches!(result, Err(ToolError::Other(_))));
}

#[tokio::test]
async fn edit_missing_old_string_field() {
    let dir = setup_workspace();
    let tool = EditTool::new();
    let c = ctx(dir.path());
    let result = tool
        .execute(&c, json!({"file_path": "f.txt", "new_string": "b"}))
        .await;
    assert!(matches!(result, Err(ToolError::Other(_))));
}

#[tokio::test]
async fn edit_missing_new_string_field() {
    let dir = setup_workspace();
    let tool = EditTool::new();
    let c = ctx(dir.path());
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
    let tool = EditTool::new();
    let c = ctx(dir.path());
    let out = tool
        .execute(
            &c,
            json!({
                "file_path": "f.txt",
                "old_string": "line2\nline3",
                "new_string": "REPLACED"
            }),
        )
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
    let tool = EditTool::new();
    assert_eq!(tool.name(), "Edit");
    assert!(tool.description().contains("replace"));
    let json = serde_json::to_value(tool.input_schema()).unwrap();
    assert!(json["properties"]["old_string"].is_object());
    assert!(json["properties"]["new_string"].is_object());
    assert!(json["properties"]["replace_all"].is_object());
}
