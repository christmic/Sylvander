//! L4 — `AutoCompact`: fork an LLM call to summarize the entire
//! conversation when context budget is exhausted. The last few
//! turns are preserved verbatim; everything older is replaced by
//! a single summary message.

use std::future::Future;
use std::pin::Pin;

use serde_json::json;
use sylvander_llm_anthropic::api::types::{MessageParam, MessageRole, UserContent};

use crate::compress::CompressContext;
use crate::compress::error::{CompactionError, CompactionFailureCode};
use crate::compress::layer::{CompressionLayer, LayerReport};

/// Default trigger ratio (matches Claude Code).
pub const DEFAULT_TRIGGER_RATIO: f32 = 0.93;

/// Default number of recent turns to preserve verbatim.
pub const DEFAULT_KEEP_LAST_N_TURNS: usize = 2;

/// L4 layer: LLM-driven summarization.
#[derive(Debug, Clone)]
pub struct AutoCompactLayer {
    pub trigger_ratio: f32,
    pub keep_last_n_turns: usize,
}

impl Default for AutoCompactLayer {
    fn default() -> Self {
        Self {
            trigger_ratio: DEFAULT_TRIGGER_RATIO,
            keep_last_n_turns: DEFAULT_KEEP_LAST_N_TURNS,
        }
    }
}

impl AutoCompactLayer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_trigger_ratio(mut self, r: f32) -> Self {
        self.trigger_ratio = r;
        self
    }

    #[must_use]
    pub fn with_keep_last_n_turns(mut self, n: usize) -> Self {
        self.keep_last_n_turns = n;
        self
    }

    fn summary_message(summary: &str) -> MessageParam {
        MessageParam {
            role: MessageRole::User,
            content: UserContent::String(format!("[Earlier conversation summary]\n{summary}")),
        }
    }
}

impl CompressionLayer for AutoCompactLayer {
    fn name(&self) -> &'static str {
        "auto_compact"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut CompressContext<'_>,
    ) -> Pin<Box<dyn Future<Output = LayerReport> + Send + 'a>> {
        Box::pin(async move {
            let used = ctx.last_usage.total_input_tokens();
            let threshold = (ctx.model_info.context_window as f32 * self.trigger_ratio) as u32;
            if used < threshold {
                return LayerReport::noop(self.name());
            }

            let Some(llm) = ctx.auto_compact_llm else {
                return LayerReport::failed_with(
                    self.name(),
                    CompactionError::new(CompactionFailureCode::UnsupportedBackend),
                );
            };

            let keep_count = (self.keep_last_n_turns * 2).min(ctx.messages.len());
            if keep_count >= ctx.messages.len() {
                return LayerReport::noop(self.name());
            }
            let split_at = ctx.messages.len() - keep_count;

            let to_summarize: Vec<MessageParam> = ctx.messages[..split_at].to_vec();
            let kept: Vec<MessageParam> = ctx.messages[split_at..].to_vec();

            let summary = match llm.summarize(&to_summarize, ctx.model_info).await {
                Ok(s) => s,
                Err(e) => {
                    return LayerReport::failed_with(self.name(), CompactionError::from_loop(&e));
                }
            };

            let summary_msg = Self::summary_message(&summary);
            let mut new_messages = Vec::with_capacity(1 + kept.len());
            new_messages.push(summary_msg);
            new_messages.extend(kept);

            let original_chars: usize = to_summarize.iter().map(|m| format!("{m:?}").len()).sum();
            let new_chars = summary.len() + 80;
            let saved = original_chars.saturating_sub(new_chars);
            let freed_tokens = (saved / 4) as u32;

            let removed_count = to_summarize.len();
            *ctx.messages = new_messages;

            LayerReport {
                name: self.name().to_string(),
                removed_count,
                condensed_count: 0,
                freed_tokens,
                details: Some(json!({
                    "summary": summary.clone(),
                    "summary_chars": summary.len(),
                    "kept_messages": keep_count,
                    "trigger_threshold": threshold,
                    "actual_used": used,
                })),
                failure: None,
                failure_code: None,
            }
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/compress_layers_auto_compact.rs"]
mod tests;
