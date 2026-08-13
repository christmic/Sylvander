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
//! `ContentBlock` directly because L1 operates on the normalized, re-feedable
//! conversation representation.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;

use sylvander_llm_core::{ChatRole, ContentBlock};

use crate::context::compression::CompressContext;
use crate::context::compression::layer::{CompressionLayer, LayerReport};

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
            if !matches!(msg.role, ChatRole::Assistant) {
                continue;
            }
            for block in &msg.content {
                if let ContentBlock::ToolCall { id, .. } = block {
                    tool_use_ids.insert(id.clone());
                }
            }
        }

        // Pass 2: drop tool_result blocks whose tool_use_id is not in the set.
        let mut removed = 0usize;
        for msg in ctx.messages.iter_mut() {
            let before = msg.content.len();
            msg.content.retain(|block| match block {
                ContentBlock::ToolResult { call_id, .. } => tool_use_ids.contains(call_id),
                _ => true,
            });
            removed += before - msg.content.len();
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

#[cfg(test)]
#[path = "../../../../tests/unit/compress_layers_orphan_snip.rs"]
mod tests;
