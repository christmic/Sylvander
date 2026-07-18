//! L1 — `OrphanSnip`: drop `tool_result` blocks whose `tool_use_id`
//! has no matching `tool_use` anywhere in the conversation.
//!
//! Orphans happen when:
//! - A `tool_result` is from a re-feed that got truncated by an
//!   earlier compression but the original `tool_use` was lost
//! - The agent loop was interrupted mid-flight
//! - A `tool_use` was emitted but the corresponding `tool_result`
//!   never made it back (rare, defensive)
//!
//! Orphan `tool_result`s are dangerous: the model sees a result
//! referencing a tool call it doesn't know about, which produces
//! hallucinated explanations. L1 silently removes them.
//!
//! ## Note on `tool_use` discovery
//!
//! Assistant `ContentBlock::ToolUse` blocks are converted (in
//! `assistant_message_from_response`) to
//! `UserContentBlock::Other(json)` with `type: "tool_use"` and
//! `id: <tool_use_id>`. L1 reads those via that JSON shape, not via
//! `ContentBlock` directly — because by the time L1 sees the
//! messages, they've already been re-fed as `MessageParam`.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;

use serde_json::Value as JsonValue;
use sylvander_llm_anthropic::api::types::{MessageRole, UserContent, UserContentBlock};

use crate::compress::CompressContext;
use crate::compress::layer::{CompressionLayer, LayerReport};

/// L1 layer: drop orphan `tool_result` blocks.
#[derive(Debug, Default, Clone, Copy)]
pub struct OrphanSnipLayer;

impl OrphanSnipLayer {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl CompressionLayer for OrphanSnipLayer {
    fn name(&self) -> &'static str {
        "orphan_snip"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut CompressContext<'_>,
    ) -> Pin<Box<dyn Future<Output = LayerReport> + Send + 'a>> {
        // Pass 1: collect every tool_use_id the model has ever emitted.
        let mut tool_use_ids: HashSet<String> = HashSet::new();
        for msg in ctx.messages.iter() {
            if !matches!(msg.role, MessageRole::Assistant) {
                continue;
            }
            let UserContent::Blocks(blocks) = &msg.content else {
                continue;
            };
            for block in blocks {
                if let Some(id) = extract_tool_use_id(block) {
                    tool_use_ids.insert(id);
                }
            }
        }

        // Pass 2: drop tool_result blocks whose tool_use_id is not in the set.
        let mut removed = 0usize;
        for msg in ctx.messages.iter_mut() {
            let UserContent::Blocks(blocks) = &mut msg.content else {
                continue;
            };
            let before = blocks.len();
            blocks.retain(|block| match block {
                UserContentBlock::ToolResult(trb) => tool_use_ids.contains(&trb.tool_use_id),
                _ => true,
            });
            removed += before - blocks.len();
        }

        let report = if removed == 0 {
            LayerReport::noop(self.name())
        } else {
            LayerReport {
                name: self.name().to_string(),
                removed_count: 0, // inner-block removals count as condensed
                condensed_count: removed,
                freed_tokens: (removed as u32) * 100, // rough heuristic
                details: None,
                failure: None,
                failure_code: None,
            }
        };
        Box::pin(async move { report })
    }
}

/// Extract `tool_use_id` from a `UserContentBlock` if it represents
/// an assistant `tool_use` (stored as `Other(json)` with
/// `type: "tool_use"` after `assistant_message_from_response`).
fn extract_tool_use_id(block: &UserContentBlock) -> Option<String> {
    let UserContentBlock::Other(json) = block else {
        return None;
    };
    if json.get("type").and_then(JsonValue::as_str) != Some("tool_use") {
        return None;
    }
    json.get("id")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

#[cfg(test)]
#[path = "../../../tests/unit/compress_layers_orphan_snip.rs"]
mod tests;
