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
mod tests {
    use super::*;
    use sylvander_llm_anthropic::api::model::ModelInfo;
    use sylvander_llm_anthropic::api::types::{
        MessageParam, ToolResultBlock, Usage, UserContent, UserContentBlock,
    };

    fn model() -> ModelInfo {
        ModelInfo::builder()
            .id("test")
            .context_window(200_000)
            .max_output_tokens(8192)
            .build()
            .unwrap()
    }

    fn usage() -> Usage {
        Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        }
    }

    fn user_with_tool_result(tool_use_id: &str) -> MessageParam {
        MessageParam {
            role: MessageRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::ToolResult(ToolResultBlock::new(
                tool_use_id,
                "result",
            ))]),
        }
    }

    fn assistant_with_tool_use(tool_use_id: &str) -> MessageParam {
        MessageParam {
            role: MessageRole::Assistant,
            content: UserContent::Blocks(vec![UserContentBlock::Other(serde_json::json!({
                "type": "tool_use",
                "id": tool_use_id,
                "name": "fake_tool",
                "input": {}
            }))]),
        }
    }

    #[tokio::test]
    async fn removes_tool_result_with_no_matching_tool_use() {
        let layer = OrphanSnipLayer::new();
        let mut messages = vec![user_with_tool_result("orphan_id")];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 1);
        // Block was removed; message is now empty.
        let UserContent::Blocks(blocks) = &messages[0].content else {
            panic!("expected blocks");
        };
        assert!(blocks.is_empty());
    }

    #[tokio::test]
    async fn keeps_tool_result_with_matching_tool_use() {
        let layer = OrphanSnipLayer::new();
        let mut messages = vec![
            assistant_with_tool_use("paired_id"),
            user_with_tool_result("paired_id"),
        ];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 0);
        let UserContent::Blocks(blocks) = &messages[1].content else {
            panic!("expected blocks");
        };
        assert_eq!(blocks.len(), 1);
    }

    #[tokio::test]
    async fn removes_multiple_orphans_in_one_pass() {
        let layer = OrphanSnipLayer::new();
        let mut messages = vec![
            user_with_tool_result("orphan_1"),
            user_with_tool_result("paired"),
            assistant_with_tool_use("paired"),
            user_with_tool_result("orphan_2"),
        ];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 2);
        // The paired one remains.
        let UserContent::Blocks(blocks) = &messages[1].content else {
            panic!("expected blocks");
        };
        assert_eq!(blocks.len(), 1);
    }

    #[tokio::test]
    async fn empty_conversation_is_noop() {
        let layer = OrphanSnipLayer::new();
        let mut messages: Vec<MessageParam> = vec![];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 0);
        assert_eq!(report.removed_count, 0);
    }
}
