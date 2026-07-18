use super::*;
use crate::tools::memory::InMemoryMemoryStore;

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
    let tool = MemoryWriteTool::new(test_store());
    let _c = ctx();
    assert_eq!(tool.name(), "write_memory");
    assert!(!tool.description().is_empty());
}

#[tokio::test]
async fn input_schema_has_content_field() {
    let tool = MemoryWriteTool::new(test_store());
    let _c = ctx();
    let schema = tool.input_schema();
    let props = schema.schema.get("properties").expect("has properties");
    assert!(props.get("content").is_some());
    assert_eq!(schema.schema["additionalProperties"], json!(false));
    for server_owned in ["owner", "scope", "id", "created_at", "provenance"] {
        assert!(props.get(server_owned).is_none());
    }
    let required = schema.schema.get("required").expect("has required");
    assert!(
        required
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("content"))
    );
}

#[tokio::test]
async fn execute_stores_and_can_search() {
    let store = test_store();
    let tool = MemoryWriteTool::new(store.clone());
    let c = ctx();

    let result = tool
        .execute(
            &c,
            json!({
                "content": "The user prefers tabs over spaces",
                "tags": ["preference", "code-style"]
            }),
        )
        .await
        .expect("execute");

    assert!(!result.is_error);
    assert!(result.content.contains("stored"));

    // Verify it was actually stored
    let results = store
        .search_relationship(
            c.memory_context(),
            "tabs over spaces",
            crate::tools::memory::MemoryFilter::default(),
        )
        .await
        .expect("search");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, "The user prefers tabs over spaces");
    assert_eq!(results[0].tags, ["preference", "code-style"]);
}

#[tokio::test]
async fn execute_missing_content_is_error() {
    let tool = MemoryWriteTool::new(test_store());
    let _c = ctx();
    let c = ctx();
    let result = tool.execute(&c, json!({"tags": ["test"]})).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn execute_without_tags_stores_cleanly() {
    let store = test_store();
    let tool = MemoryWriteTool::new(store.clone());
    let c = ctx();

    let result = tool
        .execute(&c, json!({"content": "minimal entry"}))
        .await
        .expect("execute");

    assert!(!result.is_error);

    let results = store
        .search_relationship(
            c.memory_context(),
            "minimal entry",
            crate::tools::memory::MemoryFilter::default(),
        )
        .await
        .expect("search");
    assert_eq!(results.len(), 1);
    assert!(results[0].metadata.is_empty());
}

#[tokio::test]
async fn model_input_cannot_submit_server_owned_fields() {
    let store = test_store();
    let tool = MemoryWriteTool::new(store.clone());
    let c = ctx();
    for forbidden in ["owner", "scope", "id", "created_at", "provenance"] {
        let mut input = json!({"content": "must not persist"});
        input[forbidden] = json!("attacker-controlled");
        assert!(tool.execute(&c, input).await.is_err());
    }

    let results = store
        .search_relationship(
            c.memory_context(),
            "",
            crate::tools::memory::MemoryFilter::default(),
        )
        .await
        .unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn caller_built_tool_context_cannot_forge_memory_authority() {
    let store = test_store();
    let tool = MemoryWriteTool::new(store.clone());
    let forged = ToolContext::new(sylvander_protocol::SessionContext::new(
        "victim", "agent", "session",
    ))
    .with_capability(crate::tool_context::Cap::MemoryWrite);

    let error = tool
        .execute(&forged, json!({"content": "forged trusted memory"}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("memory access denied"));

    assert!(
        store
            .search_relationship(
                ctx().memory_context(),
                "forged trusted memory",
                crate::tools::memory::MemoryFilter::default(),
            )
            .await
            .unwrap()
            .is_empty()
    );
}
