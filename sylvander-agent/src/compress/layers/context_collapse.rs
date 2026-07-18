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
#[path = "../../../tests/unit/compress_layers_context_collapse.rs"]
mod tests;
