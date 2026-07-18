use super::*;
use sylvander_protocol::SessionContext;

fn context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(SessionContext::new("user", "agent", "session"))
        .with_fs_root(root)
        .with_capability(Cap::Read)
}

#[tokio::test]
async fn searches_with_structured_results_and_explicit_truncation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("one.txt"), "crab one\ncrab two\n").unwrap();
    let output = SearchTool::new("/")
        .execute(
            &context(dir.path()),
            json!({"query": "crab", "max_results": 1}),
        )
        .await
        .unwrap();
    assert!(!output.is_error);
    let value: JsonValue = serde_json::from_str(&output.content).unwrap();
    assert_eq!(value["matches"].as_array().unwrap().len(), 1);
    assert_eq!(value["matches"][0]["line_number"], 1);
    assert_eq!(value["truncated"], true);
}

#[tokio::test]
async fn rejects_unknown_or_empty_input() {
    let dir = tempfile::tempdir().unwrap();
    let tool = SearchTool::new("/");
    assert!(
        tool.execute(&context(dir.path()), json!({"query": "", "glob": "*"}))
            .await
            .is_err()
    );
    assert!(
        tool.execute(&context(dir.path()), json!({"query": ""}))
            .await
            .is_err()
    );
}

#[test]
fn schema_is_strict_and_bounded() {
    let schema = SearchTool::new("/").input_schema();
    assert_eq!(schema.schema["additionalProperties"], false);
    assert_eq!(schema.schema["required"], json!(["query"]));
    assert_eq!(
        schema.schema["properties"]["max_results"]["maximum"],
        MAX_QUERY_RESULTS
    );
}
