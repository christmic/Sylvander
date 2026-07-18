use super::*;
use sylvander_protocol::SessionContext;

fn context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(SessionContext::new("user", "agent", "session"))
        .with_fs_root(root)
        .with_capability(Cap::Read)
}

#[tokio::test]
async fn lists_recursively_with_explicit_truncation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "fn crab() {}\n").unwrap();
    let output = ListTool::new("/")
        .execute(
            &context(dir.path()),
            json!({"path": ".", "recursive": true, "max_results": 1}),
        )
        .await
        .unwrap();
    assert!(!output.is_error);
    let value: JsonValue = serde_json::from_str(&output.content).unwrap();
    assert_eq!(value["entries"].as_array().unwrap().len(), 1);
    assert_eq!(value["truncated"], true);
}

#[test]
fn schema_and_runtime_reject_unbounded_inputs() {
    let schema = ListTool::new("/").input_schema();
    assert_eq!(schema.schema["additionalProperties"], false);
    assert_eq!(
        schema.schema["properties"]["max_results"]["maximum"],
        MAX_QUERY_RESULTS
    );
    assert!(parse_max_results(Some(&json!(MAX_QUERY_RESULTS + 1))).is_err());
    assert!(strict_object(&json!({"shell": "ls"}), &["path"]).is_err());
}
