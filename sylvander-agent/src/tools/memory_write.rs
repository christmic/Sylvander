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

use super::memory::{MemoryAppend, MemoryStore};

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
        let mut schema = InputSchema::new_with_properties(
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
        );
        schema.schema["additionalProperties"] = JsonValue::Bool(false);
        schema
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(crate::tool_context::Cap::MemoryWrite) {
            return Ok(ToolOutput::err("memory write capability not granted"));
        }
        reject_unknown_fields(&input, &["content", "tags"])?;
        let content = input["content"]
            .as_str()
            .ok_or_else(|| ToolError::Other("missing 'content' field".into()))?;

        let mut append = MemoryAppend::new(content);

        // Parse optional tags
        if let Some(tags) = input["tags"].as_array() {
            for tag in tags {
                if let Some(tag_str) = tag.as_str() {
                    append = append.with_tag(tag_str);
                }
            }
        }

        let entry = self
            .store
            .append_relationship(ctx.memory_context(), append)
            .await
            .map_err(|e| ToolError::Other(format!("memory write failed: {e}")))?;

        Ok(ToolOutput::ok(
            json!({"status": "stored", "id": entry.id}).to_string(),
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../../tests/unit/tools_memory_write.rs"]
mod tests;
