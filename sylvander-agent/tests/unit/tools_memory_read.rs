use super::*;
use crate::tools::memory::{InMemoryMemoryStore, MemoryAppend};

use crate::tool_context::ToolContext;
fn ctx() -> ToolContext {
    ToolContext::application(sylvander_protocol::SessionContext::new("u", "a", "s"))
        .with_capability(crate::tool_context::Cap::Read)
        .with_capability(crate::tool_context::Cap::Write)
        .with_capability(crate::tool_context::Cap::MemoryRead)
        .with_capability(crate::tool_context::Cap::MemoryWrite)
}

fn test_store() -> Arc<dyn MemoryStore> {
    Arc::new(InMemoryMemoryStore::new())
}

#[tokio::test]
async fn name_and_description() {
    let tool = MemoryReadTool::new(test_store());
    let _c = ctx();
    assert_eq!(tool.name(), "read_memory");
    assert!(!tool.description().is_empty());
}

#[tokio::test]
async fn input_schema_has_query_field() {
    let tool = MemoryReadTool::new(test_store());
    let _c = ctx();
    let schema = tool.input_schema();
    let props = schema.schema.get("properties").expect("has properties");
    assert!(props.get("query").is_some());
    assert_eq!(schema.schema["additionalProperties"], json!(false));
    let required = schema.schema.get("required").expect("has required");
    assert!(
        required
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("query"))
    );
}

#[tokio::test]
async fn execute_returns_matching_entries() {
    let store = test_store();
    let c = ctx();
    store
        .append_relationship(
            c.memory_context(),
            MemoryAppend::new("User prefers dark mode"),
        )
        .await
        .expect("store");
    store
        .append_relationship(c.memory_context(), MemoryAppend::new("Project uses Rust"))
        .await
        .expect("store");

    let tool = MemoryReadTool::new(store);
    let c = ctx();
    let result = tool
        .execute(&c, json!({"query": "dark mode"}))
        .await
        .expect("execute");

    assert!(!result.is_error);
    assert!(result.content.contains("dark mode"));
}

#[tokio::test]
async fn execute_missing_query_is_error() {
    let tool = MemoryReadTool::new(test_store());
    let c = ctx();
    let result = tool.execute(&c, json!({})).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn execute_rejects_unknown_top_level_fields() {
    let tool = MemoryReadTool::new(test_store());
    let c = ctx();
    for input in [
        json!({"query": "", "owner": "attacker"}),
        json!({"query": "", "scope": "relationship"}),
        json!({"query": "", "unexpected": true}),
    ] {
        assert!(tool.execute(&c, input).await.is_err());
    }
}

#[tokio::test]
async fn malformed_filters_never_expand_the_query() {
    let tool = MemoryReadTool::new(test_store());
    let c = ctx();
    for input in [
        json!({"query": "", "kind": "everything"}),
        json!({"query": "", "min_importance": "any"}),
        json!({"query": "", "limit": -1}),
        json!({"query": "", "limit": super::super::memory::MAX_MEMORY_RESULTS + 1}),
    ] {
        assert!(tool.execute(&c, input).await.is_err());
    }
}

#[tokio::test]
async fn execute_no_matches_returns_empty_array() {
    let store = test_store();
    let c = ctx();
    store
        .append_relationship(c.memory_context(), MemoryAppend::new("some content"))
        .await
        .expect("store");

    let tool = MemoryReadTool::new(store);
    let c = ctx();
    let result = tool
        .execute(&c, json!({"query": "nonexistent"}))
        .await
        .expect("execute");

    assert!(!result.is_error);
    assert!(result.content.contains("[]"));
}
