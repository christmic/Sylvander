//! `write_memory` tool — lets the model store information in long-term memory.
//!
//! The model can call this tool to persist information that should be
//! available in future conversations.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

use super::memory::{MemoryEntry, MemoryStore};

/// Tool that lets the model write to its long-term memory.
///
/// The model decides *when* and *what* to store — memories are not
/// extracted automatically. This gives the model agency and keeps
/// storage intentional.
pub struct MemoryWriteTool {
    store: Arc<dyn MemoryStore>,
}

impl MemoryWriteTool {
    /// Create a new `write_memory` tool backed by the given store.
    #[must_use]
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &'static str {
        "write_memory"
    }

    fn description(&self) -> &'static str {
        "Store a piece of information in your long-term memory. \
         Use this to remember user preferences, important decisions, \
         project-specific facts, or anything else that should persist \
         across conversations. \
         Each entry can have optional tags for categorization."
    }

    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            serde_json::json!({
                "content": {
                    "type": "string",
                    "description": "The information to store. Be concise but include enough context to be useful later."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional categorization tags (e.g. \"preference\", \"project\", \"decision\")."
                }
            }),
            &["content"],
        )
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(crate::tool_context::Cap::MemoryWrite) {
            return Ok(ToolOutput::err("memory write capability not granted"));
        }
        let content = input["content"]
            .as_str()
            .ok_or_else(|| ToolError::Other("missing 'content' field".into()))?;

        let mut entry = MemoryEntry::new(
            uuid::Uuid::new_v4().to_string(),
            content,
            ctx.session.as_ref().clone(),
        );

        // Parse optional tags
        if let Some(tags) = input["tags"].as_array() {
            for tag in tags {
                if let Some(tag_str) = tag.as_str() {
                    entry = entry.with_tag(tag_str, "true");
                }
            }
        }

        self.store
            .store(&ctx.session, entry.clone())
            .await
            .map_err(|e| ToolError::Other(format!("memory write failed: {e}")))?;

        Ok(ToolOutput::ok(
            json!({"status": "stored", "id": entry.id}).to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::InMemoryMemoryStore;

    use crate::tool_context::ToolContext;
    fn ctx() -> ToolContext {
        ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
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
            .search(
                &c.session,
                "tabs over spaces",
                crate::tools::memory::MemoryFilter::default(),
            )
            .await
            .expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "The user prefers tabs over spaces");
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
            .search(
                &c.session,
                "minimal entry",
                crate::tools::memory::MemoryFilter::default(),
            )
            .await
            .expect("search");
        assert_eq!(results.len(), 1);
        assert!(results[0].metadata.is_empty());
    }
}
