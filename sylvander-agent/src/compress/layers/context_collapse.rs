//! L3 — `ContextCollapse`: project old `thinking` blocks into stubs.
//!
//! Extended-thinking content is huge (often 1k-10k tokens per block)
//! and almost never referenced again once the assistant has moved on
//! to the next turn. Trimming it preserves the conversation
//! structure (model still sees that thinking happened, with a stable
//! signature for prompt-cache stability) while reclaiming attention
//! budget for the recent, active context.
//!
//! ## What stays
//!
//! - All `thinking` blocks in the last N assistant messages
//!   (default 3). Recent reasoning is what the model is likely
//!   building on.
//! - All non-thinking blocks (text, `tool_use`).
//! - The `signature` field on trimmed blocks — the API uses this
//!   to verify the block wasn't tampered with; replacing only the
//!   `thinking` field keeps the conversation cacheable.
//!
//! ## What gets replaced
//!
//! `thinking` field on old blocks becomes:
//! ```text
//! [...earlier reasoning omitted (N chars)]
//! ```
//!
//! All other fields (`type`, `signature`) preserved.

use std::future::Future;
use std::pin::Pin;

use serde_json::{Value as JsonValue, json};
use sylvander_llm_anthropic::api::types::{MessageRole, UserContent, UserContentBlock};

use crate::compress::CompressContext;
use crate::compress::layer::{CompressionLayer, LayerReport};

/// Default number of recent assistant messages to keep thinking
/// intact.
pub const DEFAULT_KEEP_LAST_N: usize = 3;

/// Default max thinking chars retained per block. Longer blocks
/// get trimmed.
pub const DEFAULT_MAX_THINKING_CHARS: usize = 500;

/// Placeholder template; embeds the original char count so the
/// model can see how much reasoning it lost (a hint that the
/// decision was deliberate, not a glitch).
const PLACEHOLDER_TEMPLATE: &str = "[...earlier reasoning omitted ({chars} chars)]";

/// L3 layer: trim `thinking` blocks in old assistant messages.
#[derive(Debug, Clone, Copy)]
pub struct ContextCollapseLayer {
    /// Number of recent assistant messages whose thinking blocks
    /// stay intact. Older assistant messages get their thinking
    /// trimmed.
    pub keep_last_n_assistant_messages: usize,
    /// Max retained chars per thinking block. Blocks longer than
    /// this are trimmed to a placeholder.
    pub max_thinking_chars: usize,
}

impl Default for ContextCollapseLayer {
    fn default() -> Self {
        Self {
            keep_last_n_assistant_messages: DEFAULT_KEEP_LAST_N,
            max_thinking_chars: DEFAULT_MAX_THINKING_CHARS,
        }
    }
}

impl ContextCollapseLayer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_keep_last_n(mut self, n: usize) -> Self {
        self.keep_last_n_assistant_messages = n;
        self
    }

    #[must_use]
    pub fn with_max_thinking_chars(mut self, n: usize) -> Self {
        self.max_thinking_chars = n;
        self
    }

    /// Build a placeholder string with the original char count.
    fn placeholder(original_chars: usize) -> String {
        PLACEHOLDER_TEMPLATE.replace("{chars}", &original_chars.to_string())
    }
}

impl CompressionLayer for ContextCollapseLayer {
    fn name(&self) -> &'static str {
        "context_collapse"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut CompressContext<'_>,
    ) -> Pin<Box<dyn Future<Output = LayerReport> + Send + 'a>> {
        // Walk assistant messages, count them, identify which are
        // "old" (index <= total - keep_last_n).
        let total_assistant: usize = ctx
            .messages
            .iter()
            .filter(|m| matches!(m.role, MessageRole::Assistant))
            .count();
        let active_threshold = total_assistant.saturating_sub(self.keep_last_n_assistant_messages);

        let mut condensed = 0usize;
        let mut freed_tokens = 0u32;
        let mut assistant_seen = 0usize;

        for msg in ctx.messages.iter_mut() {
            if !matches!(msg.role, MessageRole::Assistant) {
                continue;
            }
            assistant_seen += 1;
            if assistant_seen > active_threshold {
                continue;
            }

            let UserContent::Blocks(blocks) = &mut msg.content else {
                continue;
            };

            for block in blocks.iter_mut() {
                let UserContentBlock::Other(json_value) = block else {
                    continue;
                };
                if json_value.get("type").and_then(JsonValue::as_str) != Some("thinking") {
                    continue;
                }
                let Some(thinking_text) = json_value.get("thinking").and_then(JsonValue::as_str)
                else {
                    continue;
                };
                let original_len = thinking_text.len();
                if original_len <= self.max_thinking_chars {
                    continue;
                }

                let placeholder = Self::placeholder(original_len);
                let saved = original_len.saturating_sub(placeholder.len());
                freed_tokens = freed_tokens.saturating_add((saved / 4) as u32);

                // Replace just the `thinking` field. Keep `type` and
                // `signature` so the API still validates the block.
                if let Some(obj) = json_value.as_object_mut() {
                    obj.insert("thinking".to_string(), json!(placeholder));
                }
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
                details: Some(json!({
                    "trimmed_blocks": condensed,
                    "max_thinking_chars": self.max_thinking_chars,
                })),
                failure: None,
                failure_code: None,
            }
        };
        Box::pin(async move { report })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylvander_llm_anthropic::api::model::ModelInfo;
    use sylvander_llm_anthropic::api::types::{MessageParam, Usage};

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

    fn assistant_with_thinking(thinking: &str) -> MessageParam {
        MessageParam {
            role: MessageRole::Assistant,
            content: UserContent::Blocks(vec![UserContentBlock::Other(json!({
                "type": "thinking",
                "thinking": thinking,
                "signature": "sig_xyz"
            }))]),
        }
    }

    fn extract_thinking(msg: &MessageParam) -> Option<String> {
        let UserContent::Blocks(blocks) = &msg.content else {
            return None;
        };
        let UserContentBlock::Other(j) = blocks.first()? else {
            return None;
        };
        j.get("thinking")
            .and_then(JsonValue::as_str)
            .map(str::to_string)
    }

    #[tokio::test]
    async fn trims_old_thinking_blocks() {
        let layer = ContextCollapseLayer::new()
            .with_keep_last_n(1)
            .with_max_thinking_chars(100);
        let long_thinking = "x".repeat(500);
        let mut messages = vec![
            assistant_with_thinking(&long_thinking),
            assistant_with_thinking(&long_thinking),
            assistant_with_thinking(&long_thinking),
        ];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        // The 2 oldest get trimmed (keep_last_n=1).
        assert_eq!(report.condensed_count, 2);
        assert!(report.freed_tokens > 0);

        let s0 = extract_thinking(&messages[0]).unwrap();
        assert!(s0.contains("earlier reasoning omitted"));
        assert!(s0.contains("500"));

        let s2 = extract_thinking(&messages[2]).unwrap();
        // The most recent stays intact.
        assert_eq!(s2, long_thinking);
    }

    #[tokio::test]
    async fn preserves_short_thinking() {
        let layer = ContextCollapseLayer::new()
            .with_keep_last_n(0)
            .with_max_thinking_chars(100);
        let short = "brief reasoning";
        let mut messages = vec![assistant_with_thinking(short)];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 0);
        // Short thinking stays as-is.
        assert_eq!(extract_thinking(&messages[0]).unwrap(), short);
    }

    #[tokio::test]
    async fn preserves_signature_field() {
        // The signature field must survive — the API uses it to
        // verify the thinking block.
        let layer = ContextCollapseLayer::new()
            .with_keep_last_n(0)
            .with_max_thinking_chars(50);
        let long = "y".repeat(500);
        let mut messages = vec![assistant_with_thinking(&long)];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        layer.apply(&mut ctx).await;
        let UserContent::Blocks(blocks) = &messages[0].content else {
            panic!();
        };
        let UserContentBlock::Other(j) = &blocks[0] else {
            panic!();
        };
        assert_eq!(
            j.get("signature").and_then(JsonValue::as_str),
            Some("sig_xyz")
        );
        assert_eq!(j.get("type").and_then(JsonValue::as_str), Some("thinking"));
    }

    #[tokio::test]
    async fn empty_conversation_is_noop() {
        let layer = ContextCollapseLayer::new();
        let mut messages: Vec<MessageParam> = vec![];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 0);
        assert!(report.failure.is_none());
    }

    #[tokio::test]
    async fn user_messages_with_other_content_untouched() {
        // User messages shouldn't have thinking blocks, but verify
        // L3 doesn't accidentally damage user tool_result blocks
        // that happen to be wrapped in Other(json).
        let layer = ContextCollapseLayer::new()
            .with_keep_last_n(0)
            .with_max_thinking_chars(50);
        let mut messages = vec![MessageParam {
            role: MessageRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Other(json!({
                "type": "tool_use",
                "id": "toolu_x",
                "name": "fake",
                "input": {}
            }))]),
        }];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.condensed_count, 0);
        // The tool_use block is intact.
        let UserContent::Blocks(blocks) = &messages[0].content else {
            panic!();
        };
        let UserContentBlock::Other(j) = &blocks[0] else {
            panic!();
        };
        assert_eq!(j.get("type").and_then(JsonValue::as_str), Some("tool_use"));
    }
}
