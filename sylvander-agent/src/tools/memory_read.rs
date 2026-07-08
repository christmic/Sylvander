//! `read_memory` tool — lets the model search its long-term memory.
//!
//! The model can call this tool with a search query to retrieve
//! relevant memories. Results are returned as a JSON array.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value as JsonValue};

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};

use super::memory::MemoryStore;

/// Tool that lets the model query its long-term memory.
///
/// The model decides *when* to read memory — it's not injected into
/// the system prompt. This keeps the prompt lean and gives the model
/// agency over what context it retrieves.
pub struct MemoryReadTool {
    store: Arc<dyn MemoryStore>,
}

impl MemoryReadTool {
    /// Create a new `read_memory` tool backed by the given store.
    #[must_use]
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for MemoryReadTool {
    fn name(&self) -> &str {
        "read_memory"
    }

    fn description(&self) -> &str {
        "Search your long-term memory for relevant information. \
         Use this when you need to recall user preferences, \
         project-specific context, or past decisions that are not \
         in the current conversation. \
         The results are returned as a JSON array of matching entries."
    }

    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            serde_json::json!({
                "query": {
                    "type": "string",
                    "description": "Search query. Use keywords or phrases to find relevant memories. Case-insensitive."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5)."
                }
            }),
            &["query"],
        )
    }

    async fn execute(&self, input: JsonValue) -> Result<ToolOutput, ToolError> {
        let query = input["query"]
            .as_str()
            .ok_or_else(|| ToolError::Other("missing 'query' field".into()))?;

        let limit = input["limit"].as_u64().unwrap_or(5) as usize;

        let results = self
            .store
            .search(query, limit)
            .await
            .map_err(|e| ToolError::Other(format!("memory search failed: {e}")))?;

        let json_results: Vec<JsonValue> = results
            .iter()
            .map(|entry| {
                json!({
                    "id": entry.id,
                    "content": entry.content,
                    "metadata": entry.metadata,
                })
            })
            .collect();

        Ok(ToolOutput::ok(serde_json::to_string_pretty(
            &json_results,
        )
        .unwrap_or_else(|_| format!("{json_results:#?}"))))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::{InMemoryMemoryStore, MemoryEntry};

    fn test_store() -> Arc<dyn MemoryStore> {
        Arc::new(InMemoryMemoryStore::new())
    }

    #[tokio::test]
    async fn name_and_description() {
        let tool = MemoryReadTool::new(test_store());
        assert_eq!(tool.name(), "read_memory");
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn input_schema_has_query_field() {
        let tool = MemoryReadTool::new(test_store());
        let schema = tool.input_schema();
        let props = schema.schema.get("properties").expect("has properties");
        assert!(props.get("query").is_some());
        let required = schema.schema.get("required").expect("has required");
        assert!(required.as_array().unwrap().contains(&serde_json::json!("query")));
    }

    #[tokio::test]
    async fn execute_returns_matching_entries() {
        let store = test_store();
        store
            .store(MemoryEntry::new("1", "User prefers dark mode"))
            .await
            .expect("store");
        store
            .store(MemoryEntry::new("2", "Project uses Rust"))
            .await
            .expect("store");

        let tool = MemoryReadTool::new(store);
        let result = tool
            .execute(json!({"query": "dark mode"}))
            .await
            .expect("execute");

        assert!(!result.is_error);
        assert!(result.content.contains("dark mode"));
    }

    #[tokio::test]
    async fn execute_missing_query_is_error() {
        let tool = MemoryReadTool::new(test_store());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_no_matches_returns_empty_array() {
        let store = test_store();
        store
            .store(MemoryEntry::new("1", "some content"))
            .await
            .expect("store");

        let tool = MemoryReadTool::new(store);
        let result = tool
            .execute(json!({"query": "nonexistent"}))
            .await
            .expect("execute");

        assert!(!result.is_error);
        assert!(result.content.contains("[]"));
    }
}
