//! L2 — `MicroCompact`: replace old `tool_result` blocks with a
//! placeholder. Preserves the most recent N user messages
//! verbatim; everything older gets condensed.
//!
//! ## Why
//!
//! Recent `tool_result` blocks are part of the active conversation
//! — the model is likely referring to them in its next response.
//! Old `tool_result` blocks are mostly noise: their full content
//! is rarely needed again. Replacing with a placeholder keeps
//! the conversation structure intact (so the model knows a tool
//! was called and what its id was) while shedding token cost.
//!
//! ## What stays intact
//!
//! - The last N user messages (default 3, configurable). This
//!   covers the most recent `tool_use`/`tool_result` exchanges.
//! - All assistant messages (text + thinking).
//! - All non-`ToolResult` blocks within older messages (Text,
//!   Image, Other).
//!
//! ## What's replaced
//!
//! `ToolResult` blocks in older user messages get their content
//! swapped for:
//! ```text
//! [Previous tool result for <tool_use_id> truncated; refer to earlier context.]
//! ```
//!
//! The block itself (`tool_use_id`, `is_error`, `cache_control`) stays
//! intact — only the body shrinks.

use std::future::Future;
use std::pin::Pin;

use sylvander_llm_anthropic::api::types::{
    MessageRole, ToolResultContent, UserContent, UserContentBlock,
};

use crate::compress::CompressContext;
use crate::compress::layer::{CompressionLayer, LayerReport};

/// Default number of recent user messages to keep intact.
pub const DEFAULT_KEEP_LAST_N: usize = 3;

/// Placeholder template; embeds the original `tool_use_id` so the
/// model can still correlate with the `tool_use` block.
const PLACEHOLDER_TEMPLATE: &str =
    "[Previous tool result for {tool_use_id} truncated; refer to earlier context.]";

/// L2 layer: condense old `tool_result` blocks.
#[derive(Debug, Clone, Copy)]
pub struct MicroCompactLayer {
    /// Number of recent user messages to leave untouched. Older
    /// user messages get their `tool_result` content replaced with
    /// a placeholder.
    pub keep_last_n_user_messages: usize,
}

impl Default for MicroCompactLayer {
    fn default() -> Self {
        Self {
            keep_last_n_user_messages: DEFAULT_KEEP_LAST_N,
        }
    }
}

impl MicroCompactLayer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override `keep_last_n_user_messages`.
    #[must_use]
    pub fn with_keep_last_n(mut self, n: usize) -> Self {
        self.keep_last_n_user_messages = n;
        self
    }

    /// Build a placeholder string for a given `tool_use_id`.
    fn placeholder(tool_use_id: &str) -> String {
        PLACEHOLDER_TEMPLATE.replace("{tool_use_id}", tool_use_id)
    }
}

impl CompressionLayer for MicroCompactLayer {
    fn name(&self) -> &'static str {
        "micro_compact"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut CompressContext<'_>,
    ) -> Pin<Box<dyn Future<Output = LayerReport> + Send + 'a>> {
        // Count total user messages; the last N are "active" (kept
        // verbatim). Anything before that, condense.
        let total_user: usize = ctx
            .messages
            .iter()
            .filter(|m| matches!(m.role, MessageRole::User))
            .count();
        let active_threshold = total_user.saturating_sub(self.keep_last_n_user_messages);

        let mut condensed = 0usize;
        let mut freed_tokens = 0u32;
        let mut user_seen = 0usize;

        for msg in ctx.messages.iter_mut() {
            if !matches!(msg.role, MessageRole::User) {
                continue;
            }
            user_seen += 1;
            // Active messages: user index in (active_threshold .. total_user].
            if user_seen > active_threshold {
                continue;
            }

            let UserContent::Blocks(blocks) = &mut msg.content else {
                continue;
            };
            for block in blocks.iter_mut() {
                let UserContentBlock::ToolResult(trb) = block else {
                    continue;
                };
                let Some(ToolResultContent::String(body)) = trb.content.as_ref() else {
                    continue;
                };

                let placeholder = Self::placeholder(&trb.tool_use_id);
                let original_len = body.len();
                let new_len = placeholder.len();
                if original_len <= new_len {
                    // Already shorter than placeholder — no point
                    // rewriting. Skip.
                    continue;
                }
                let saved = original_len.saturating_sub(new_len);
                freed_tokens = freed_tokens.saturating_add((saved / 4) as u32);
                trb.content = Some(ToolResultContent::String(placeholder));
                condensed += 1;
            }
        }

        let report = if condensed == 0 {
            LayerReport::noop(self.name())
        } else {
            LayerReport {
                name: self.name().to_string(),
                removed_count: 0,
                condensed_count: condensed,
                freed_tokens,
                details: None,
                failure: None,
            }
        };
        Box::pin(async move { report })
    }
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

    fn user_msg_text(text: &str) -> MessageParam {
        MessageParam {
            role: MessageRole::User,
            content: UserContent::String(text.to_string()),
        }
    }

    fn user_msg_with_tool_result(tool_use_id: &str, body: &str) -> MessageParam {
        MessageParam {
            role: MessageRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::ToolResult(ToolResultBlock::new(
                tool_use_id,
                body,
            ))]),
        }
    }

    fn first_tool_result_body(msg: &MessageParam) -> Option<String> {
        let UserContent::Blocks(blocks) = &msg.content else {
            return None;
        };
        let UserContentBlock::ToolResult(trb) = blocks.first()? else {
            return None;
        };
        match trb.content.as_ref()? {
            ToolResultContent::String(s) => Some(s.clone()),
            ToolResultContent::Blocks(_) => None,
        }
    }

    #[tokio::test]
    async fn keeps_last_n_user_messages_intact() {
        let layer = MicroCompactLayer::new().with_keep_last_n(2);
        let mut messages = vec![
            user_msg_with_tool_result("old_1", "x".repeat(200).as_str()),
            user_msg_with_tool_result("old_2", "y".repeat(200).as_str()),
            user_msg_with_tool_result("recent_1", "z".repeat(200).as_str()),
            user_msg_with_tool_result("recent_2", "w".repeat(200).as_str()),
        ];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 2);
        // The two old ones got placeholders; the two recent ones
        // are intact.
        let body0 = first_tool_result_body(&messages[0]).unwrap();
        assert!(body0.contains("truncated"), "old_1 should be condensed");
        let body2 = first_tool_result_body(&messages[2]).unwrap();
        assert!(
            !body2.contains("truncated"),
            "recent_1 should be intact, got: {body2}"
        );
    }

    #[tokio::test]
    async fn does_not_affect_user_text_messages() {
        let layer = MicroCompactLayer::new().with_keep_last_n(0);
        let mut messages = vec![user_msg_text("user plain text")];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 0);
        // User text is unchanged.
        let UserContent::String(s) = &messages[0].content else {
            panic!("expected string");
        };
        assert_eq!(s, "user plain text");
    }

    #[tokio::test]
    async fn zero_keep_condenses_all_tool_results() {
        let layer = MicroCompactLayer::new().with_keep_last_n(0);
        let mut messages = vec![
            user_msg_with_tool_result("a", "x".repeat(100).as_str()),
            user_msg_with_tool_result("b", "y".repeat(100).as_str()),
        ];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 2);
        assert!(report.freed_tokens > 0);
    }

    #[tokio::test]
    async fn empty_conversation_is_noop() {
        let layer = MicroCompactLayer::new();
        let mut messages: Vec<MessageParam> = vec![];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 0);
    }

    #[tokio::test]
    async fn short_tool_results_not_rewritten() {
        // If a tool_result body is already shorter than the
        // placeholder, don't bother rewriting.
        let layer = MicroCompactLayer::new().with_keep_last_n(0);
        let mut messages = vec![user_msg_with_tool_result("a", "short")];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 0);
    }
}
