//! `read_memory` tool — lets the model search its long-term memory.
//!
//! The model can call this tool with a search query to retrieve
//! relevant memories. Results are returned as a JSON array.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value as JsonValue};

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

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
                    "description": "Search query. Use keywords or phrases to find relevant memories. Case-insensitive. Empty string returns most recent / most important entries."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5)."
                },
                "kind": {
                    "type": "string",
                    "description": "Optional filter: only return memories of this kind ('preference', 'project_fact', 'decision', 'conversation_ref', 'agent_note')."
                },
                "min_importance": {
                    "type": "string",
                    "description": "Optional filter: minimum importance ('low', 'medium', 'high', 'critical')."
                }
            }),
            &["query"],
        )
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        input: JsonValue,
    ) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(crate::tool_context::Cap::MemoryRead) {
            return Ok(ToolOutput::err("memory read capability not granted"));
        }
        let query = input["query"]
            .as_str()
            .ok_or_else(|| ToolError::Other("missing 'query' field".into()))?;

        let limit = input["limit"].as_u64().unwrap_or(5) as usize;

        let kind_filter = parse_kind(input.get("kind").and_then(|v| v.as_str()));
        let importance_filter =
            parse_importance(input.get("min_importance").and_then(|v| v.as_str()));

        let results = self
            .store
            .search(
                &ctx.session,
                query,
                super::memory::MemoryFilter {
                    kind: kind_filter,
                    min_importance: importance_filter,
                    limit: Some(limit),
                },
            )
            .await
            .map_err(|e| ToolError::Other(format!("memory search failed: {e}")))?;

        let json_results: Vec<JsonValue> = results
            .iter()
            .map(|entry| {
                json!({
                    "id": entry.id,
                    "kind": entry.kind,
                    "importance": entry.importance,
                    "content": entry.content,
                    "tags": entry.tags,
                    "references": entry.references,
                    "created_at": entry.created_at,
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

    use crate::tool_context::ToolContext;
    fn ctx() -> ToolContext { ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s")).with_capability(crate::tool_context::Cap::Read).with_capability(crate::tool_context::Cap::Write).with_capability(crate::tool_context::Cap::MemoryRead).with_capability(crate::tool_context::Cap::MemoryWrite) }

    fn test_store() -> Arc<dyn MemoryStore> {
        Arc::new(InMemoryMemoryStore::new())
    }

    #[tokio::test]
    async fn name_and_description() {
        let tool = MemoryReadTool::new(test_store());
        let c = ctx();
        assert_eq!(tool.name(), "read_memory");
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn input_schema_has_query_field() {
        let tool = MemoryReadTool::new(test_store());
        let c = ctx();
        let schema = tool.input_schema();
        let props = schema.schema.get("properties").expect("has properties");
        assert!(props.get("query").is_some());
        let required = schema.schema.get("required").expect("has required");
        assert!(required.as_array().unwrap().contains(&serde_json::json!("query")));
    }

    #[tokio::test]
    async fn execute_returns_matching_entries() {
        let store = test_store();
        let c = ctx();
        store
            .store(&c.session, MemoryEntry::new("1", "User prefers dark mode", c.session.as_ref().clone()))
            .await
            .expect("store");
        store
            .store(&c.session, MemoryEntry::new("2", "Project uses Rust", c.session.as_ref().clone()))
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
    async fn execute_no_matches_returns_empty_array() {
        let store = test_store();
        let c = ctx();
        store
            .store(&c.session, MemoryEntry::new("1", "some content", c.session.as_ref().clone()))
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
}

// Parse a string from the model's input into a `MemoryKind`. Unknown
// values map to `None` (no filter) so the model can probe without
// hard-failing.
fn parse_kind(s: Option<&str>) -> Option<super::memory::MemoryKind> {
    let s = s?;
    Some(match s {
        "preference" => super::memory::MemoryKind::Preference,
        "project_fact" => super::memory::MemoryKind::ProjectFact,
        "decision" => super::memory::MemoryKind::Decision,
        "agent_note" => super::memory::MemoryKind::AgentNote,
        _ => return None,
    })
}

fn parse_importance(s: Option<&str>) -> Option<super::memory::Importance> {
    let s = s?;
    Some(match s {
        "low" => super::memory::Importance::Low,
        "medium" => super::memory::Importance::Medium,
        "high" => super::memory::Importance::High,
        "critical" => super::memory::Importance::Critical,
        _ => return None,
    })
}
