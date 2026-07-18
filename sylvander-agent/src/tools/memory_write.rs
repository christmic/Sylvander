//! `write_memory` tool — lets the model propose governed long-term memory.
//!
//! Runtime derives the owner and destination, then Guardian policy may reject
//! the proposal or require explicit user confirmation before committing it.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::curated_memory::{CuratedMemoryScope, MemoryCandidateSink, MemoryCandidateSubmission};
use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

use super::memory::{MemoryAppend, MemoryStore};

/// Tool that lets the model propose information for long-term memory.
///
/// The model decides *when* and *what* to propose. Runtime and Guardian retain
/// authority over ownership, policy, confirmation, and persistence.
pub struct MemoryWriteTool {
    target: MemoryWriteTarget,
}

enum MemoryWriteTarget {
    /// Explicit synchronous relationship-memory product path. Runtime
    /// composition uses `Candidate`; this path remains for trusted embedded
    /// applications that deliberately request immediate relationship storage.
    Relationship(Arc<dyn MemoryStore>),
    Candidate(Arc<dyn MemoryCandidateSink>),
}

impl MemoryWriteTool {
    /// Create an explicit synchronous relationship-memory writer.
    #[must_use]
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self {
            target: MemoryWriteTarget::Relationship(store),
        }
    }

    /// Create the production Worker candidate surface. The sink owns
    /// classification, evidence, owner derivation, and mutation delivery.
    #[must_use]
    pub fn candidate(sink: Arc<dyn MemoryCandidateSink>) -> Self {
        Self {
            target: MemoryWriteTarget::Candidate(sink),
        }
    }
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &'static str {
        "write_memory"
    }

    fn description(&self) -> &'static str {
        "Propose information for governed long-term memory. \
         Use this for user preferences, important decisions, or project facts \
         that may be useful across conversations. Runtime derives the owner, \
         and policy may reject the proposal or require user confirmation. \
         Each proposal can have optional categorization tags."
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
                },
                "scope": {
                    "type": "string",
                    "enum": ["relationship", "user_profile", "agent_canonical", "workspace_knowledge"],
                    "description": "Governed destination proposal. Ownership is always derived by the Runtime."
                }
            }),
            &["content"],
        );
        schema.schema["additionalProperties"] = JsonValue::Bool(false);
        schema
    }

    fn invocation_class(&self) -> crate::tool_invocation::ToolInvocationClass {
        crate::tool_invocation::ToolInvocationClass::MemoryCandidate
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(crate::tool_context::Cap::MemoryWrite) {
            return Ok(ToolOutput::err("memory write capability not granted"));
        }
        reject_unknown_fields(&input, &["content", "tags", "scope"])?;
        let content = input["content"]
            .as_str()
            .ok_or_else(|| ToolError::Other("missing 'content' field".into()))?;
        let scope = parse_scope(input.get("scope"))?;
        let tags = input["tags"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| ToolError::Other("memory tag must be a string".into()))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();

        match &self.target {
            MemoryWriteTarget::Relationship(store) => {
                if scope != CuratedMemoryScope::Relationship {
                    return Ok(ToolOutput::err(
                        "synchronous memory path supports relationship scope only",
                    ));
                }
                let append = tags.iter().fold(MemoryAppend::new(content), |append, tag| {
                    append.with_tag(tag)
                });
                let entry = store
                    .append_relationship(ctx.memory_context(), append)
                    .await
                    .map_err(|e| ToolError::Other(format!("memory write failed: {e}")))?;
                Ok(ToolOutput::ok(
                    json!({"status": "stored", "id": entry.id}).to_string(),
                ))
            }
            MemoryWriteTarget::Candidate(sink) => {
                let receipt = sink
                    .submit(
                        ctx,
                        MemoryCandidateSubmission {
                            scope,
                            content: content.to_owned(),
                            tags,
                        },
                    )
                    .await
                    .map_err(|error| {
                        ToolError::Other(format!("memory candidate rejected: {error}"))
                    })?;
                Ok(ToolOutput::ok(
                    json!({"status": "queued", "event_id": receipt.event_id}).to_string(),
                ))
            }
        }
    }
}

fn parse_scope(value: Option<&JsonValue>) -> Result<CuratedMemoryScope, ToolError> {
    match value.and_then(JsonValue::as_str).unwrap_or("relationship") {
        "relationship" => Ok(CuratedMemoryScope::Relationship),
        "user_profile" => Ok(CuratedMemoryScope::UserProfile),
        "agent_canonical" => Ok(CuratedMemoryScope::AgentCanonical),
        "workspace_knowledge" => Ok(CuratedMemoryScope::WorkspaceKnowledge),
        _ => Err(ToolError::Other("memory scope is invalid".into())),
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
