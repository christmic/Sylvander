//! `read_memory` tool — lets the model search its long-term memory.
//!
//! The model can call this tool with a search query to retrieve
//! relevant memories. Results are returned as a JSON array.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};

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
    fn name(&self) -> &'static str {
        "read_memory"
    }

    fn description(&self) -> &'static str {
        "Search your long-term memory for relevant information. \
         Use this when you need to recall user preferences, \
         project-specific context, or past decisions that are not \
         in the current conversation. \
         The results are returned as a JSON array of matching entries."
    }

    fn input_schema(&self) -> InputSchema {
        let mut schema = InputSchema::new_with_properties(
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
        );
        schema.schema["additionalProperties"] = JsonValue::Bool(false);
        schema
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(crate::tool_context::Cap::MemoryRead) {
            return Ok(ToolOutput::err("memory read capability not granted"));
        }
        reject_unknown_fields(&input, &["query", "limit", "kind", "min_importance"])?;
        let query = input["query"]
            .as_str()
            .ok_or_else(|| ToolError::Other("missing 'query' field".into()))?;

        let limit =
            match input.get("limit") {
                None => 5,
                Some(value) => usize::try_from(value.as_u64().ok_or_else(|| {
                    ToolError::Other("'limit' must be a positive integer".into())
                })?)
                .map_err(|_| ToolError::Other("'limit' is too large".into()))?,
            };

        let kind_filter = parse_kind(input.get("kind").and_then(|v| v.as_str()))?;
        let importance_filter =
            parse_importance(input.get("min_importance").and_then(|v| v.as_str()))?;

        let results = self
            .store
            .search_relationship(
                ctx.memory_context(),
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

        Ok(ToolOutput::ok(
            serde_json::to_string_pretty(&json_results)
                .unwrap_or_else(|_| format!("{json_results:#?}")),
        ))
    }
}

fn reject_unknown_fields(input: &JsonValue, allowed: &[&str]) -> Result<(), ToolError> {
    let object = input
        .as_object()
        .ok_or_else(|| ToolError::Other("memory tool input must be an object".into()))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ToolError::Other(
            "memory tool input contains an unknown field".into(),
        ));
    }
    Ok(())
}

fn parse_kind(s: Option<&str>) -> Result<Option<super::memory::MemoryKind>, ToolError> {
    let Some(s) = s else { return Ok(None) };
    Ok(Some(match s {
        "preference" => super::memory::MemoryKind::Preference,
        "project_fact" => super::memory::MemoryKind::ProjectFact,
        "decision" => super::memory::MemoryKind::Decision,
        "agent_note" => super::memory::MemoryKind::AgentNote,
        _ => return Err(ToolError::Other("unknown memory kind".into())),
    }))
}

fn parse_importance(s: Option<&str>) -> Result<Option<super::memory::Importance>, ToolError> {
    let Some(s) = s else { return Ok(None) };
    Ok(Some(match s {
        "low" => super::memory::Importance::Low,
        "medium" => super::memory::Importance::Medium,
        "high" => super::memory::Importance::High,
        "critical" => super::memory::Importance::Critical,
        _ => return Err(ToolError::Other("unknown memory importance".into())),
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../../tests/unit/tools_memory_read.rs"]
mod tests;
