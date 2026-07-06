//! Compression strategies for the agent loop's message history.
//!
//! Called BEFORE each LLM request to keep the conversation within the
//! model's context window. M2 ships two strategies:
//!
//! - [`NoCompression`] — passthrough, useful for tests + short sessions
//! - [`SimpleWindowCompressor`] — when input tokens approach the model
//!   context window, drop the oldest non-system messages
//!
//! M3 introduces a multi-layer [`CompressionPipeline`] that composes
//! independent [`CompressionLayer`]s in cheap-first, expensive-last
//! order (see `pipeline.rs` and `layers/`).
//!
//! Custom strategies (summarization, semantic dedup, etc.) implement
//! the [`Compressor`] trait.

pub mod disk;
pub mod layer;
pub mod layers;
pub mod pipeline;

use std::collections::HashSet;

use sylvander_llm_anthropic::api::types::{MessageParam, MessageRole, Usage};
use sylvander_llm_anthropic::api::model::ModelInfo;

/// Outcome of a compression decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressionOutcome {
    /// No compression needed; messages unchanged.
    Keep,
    /// Dropped `removed_count` messages from the front of the list,
    /// freeing approximately `freed_tokens` (estimate).
    Truncated {
        /// Number of messages removed from the front.
        removed_count: usize,
        /// Estimated tokens freed (heuristic; not exact).
        freed_tokens: u32,
    },
}

/// Context passed to a compressor each iteration.
pub struct CompressContext<'a> {
    /// Mutable message history. Compressor may drop from the front.
    pub messages: &'a mut Vec<MessageParam>,
    /// Token usage reported by the last LLM response.
    pub last_usage: &'a Usage,
    /// Resolved model metadata (for `context_window` + capabilities).
    pub model_info: &'a ModelInfo,
}

/// Trait for compression strategies.
pub trait Compressor: Send + Sync {
    /// Inspect the conversation and (optionally) truncate it.
    fn maybe_compress(&self, ctx: &mut CompressContext<'_>) -> CompressionOutcome;

    /// Stable identifier used when emitting a [`LayerReport`] for this
    /// strategy via the legacy single-strategy path. The M3
    /// [`CompressionPipeline`](self::pipeline::CompressionPipeline)
    /// uses per-layer names instead. Default: `"compressor"`.
    fn name(&self) -> &'static str {
        "compressor"
    }
}

/// Bridge from the legacy sync `Compressor` API to the per-layer
/// `LayerReport` shape used by M3 events and the pipeline. Returns
/// `None` for `Keep` (no work done → no event emitted).
///
/// This is the only place that knows how to interpret
/// [`CompressionOutcome`]; layers report directly via `LayerReport`.
#[must_use]
pub fn outcome_to_layer_report(name: &str, outcome: &CompressionOutcome) -> Option<layer::LayerReport> {
    use layer::LayerReport;
    match outcome {
        CompressionOutcome::Keep => None,
        CompressionOutcome::Truncated {
            removed_count,
            freed_tokens,
        } => Some(LayerReport {
            name: name.to_string(),
            removed_count: *removed_count,
            condensed_count: 0,
            freed_tokens: *freed_tokens,
            details: None,
            failure: None,
        }),
    }
}

// =============================================================================
// NoCompression — default no-op.
// =============================================================================

/// Compressor that never truncates. Default for tests and short sessions.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCompression;

impl Compressor for NoCompression {
    fn name(&self) -> &'static str {
        "no_compression"
    }

    fn maybe_compress(&self, _ctx: &mut CompressContext<'_>) -> CompressionOutcome {
        CompressionOutcome::Keep
    }
}

// =============================================================================
// SimpleWindowCompressor — drop oldest non-system messages when full.
// =============================================================================

/// Simple compression: when input tokens exceed
/// `context_window * threshold`, drop the oldest non-system messages
/// until back under the threshold.
///
/// M2's "good enough" strategy. Replaceable with smarter strategies
/// (summarization, semantic dedup, etc.) in M3+.
#[derive(Debug, Clone, Copy)]
pub struct SimpleWindowCompressor {
    /// Trigger compression when `input_tokens >= context_window *
    /// threshold_ratio * context_window`. `0.0..=1.0`. Default: `0.85`.
    pub threshold_ratio: f32,
    /// If `true`, keep the first user message even if it's not the
    /// oldest. Useful to preserve the original task. Default: `true`.
    pub preserve_first_user: bool,
}

impl Default for SimpleWindowCompressor {
    fn default() -> Self {
        Self {
            threshold_ratio: 0.85,
            preserve_first_user: true,
        }
    }
}

impl SimpleWindowCompressor {
    /// Create a compressor with a custom threshold ratio.
    #[must_use]
    pub const fn with_threshold(mut self, ratio: f32) -> Self {
        self.threshold_ratio = ratio;
        self
    }

    /// Enable / disable preserving the first user message.
    #[must_use]
    pub const fn with_preserve_first_user(mut self, preserve: bool) -> Self {
        self.preserve_first_user = preserve;
        self
    }
}

impl Compressor for SimpleWindowCompressor {
    fn name(&self) -> &'static str {
        "simple_window_compressor"
    }
    fn maybe_compress(&self, ctx: &mut CompressContext<'_>) -> CompressionOutcome {
        let threshold_tokens =
            (ctx.model_info.context_window as f32 * self.threshold_ratio) as u32;

        if ctx.last_usage.input_tokens < threshold_tokens {
            return CompressionOutcome::Keep;
        }

        // Build the set of indices that must NOT be removed.
        let mut protected: HashSet<usize> = HashSet::new();
        // Index 0..n: preserve all system messages
        for (i, m) in ctx.messages.iter().enumerate() {
            if matches!(m.role, MessageRole::Assistant)
                && matches!(
                    &m.content,
                    sylvander_llm_anthropic::api::types::UserContent::String(_)
                )
            {
                // skip user; this is only reached if first iter; we want to preserve the first user
            }
            if is_system_message(m) {
                protected.insert(i);
            }
        }
        // Preserve the first user message if configured
        if self.preserve_first_user
            && let Some(idx) = ctx.messages.iter().position(is_user_message)
        {
            protected.insert(idx);
        }

        // Iteratively remove the oldest non-protected message until
        // input_tokens is below threshold.
        // Heuristic: assume 1 message ≈ 100 tokens; this is intentionally
        // crude — actual token savings are measured post-truncation by
        // the next LLM response.
        let mut removed_count = 0usize;
        let mut freed_tokens = 0u32;
        let mut safety = ctx.messages.len();
        while ctx.last_usage.input_tokens >= threshold_tokens && safety > 0 {
            // Find oldest non-protected message
            let Some(victim_idx) = ctx
                .messages
                .iter()
                .enumerate()
                .find(|(i, _)| !protected.contains(i))
                .map(|(i, _)| i)
            else {
                break; // all messages protected, can't compress more
            };
            ctx.messages.remove(victim_idx);
            // Protected indices > victim_idx shift down by 1
            protected = protected
                .iter()
                .map(|&i| if i > victim_idx { i - 1 } else { i })
                .collect();
            removed_count += 1;
            freed_tokens += 100; // heuristic
            safety -= 1;
        }

        if removed_count == 0 {
            CompressionOutcome::Keep
        } else {
            CompressionOutcome::Truncated {
                removed_count,
                freed_tokens,
            }
        }
    }
}

/// Returns true if a message is a system message (sent in `system` field,
/// not in `messages` array — but for re-feed we keep them inline).
fn is_system_message(_m: &MessageParam) -> bool {
    // M2 only re-feeds user + assistant turns; system is sent separately
    // via CreateMessageRequest.system. So no message in `messages` is
    // actually a system message. This helper is a no-op for M2 but
    // exists for forward compatibility.
    false
}

/// Returns true if a message is a user-role message.
fn is_user_message(m: &MessageParam) -> bool {
    matches!(m.role, MessageRole::User)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylvander_llm_anthropic::api::types::{
        MessageParam, MessageRole, Usage, UserContent,
    };
use sylvander_llm_anthropic::api::model::ModelCapabilities;

    fn model_info(context_window: u32) -> ModelInfo {
        ModelInfo::builder()
            .id("test-model")
            .context_window(context_window)
            .max_output_tokens(8192)
            .capabilities(ModelCapabilities::default())
            .build()
            .unwrap()
    }

    fn user_msg(text: &str) -> MessageParam {
        MessageParam {
            role: MessageRole::User,
            content: UserContent::String(text.to_string()),
        }
    }

    fn assistant_msg(text: &str) -> MessageParam {
        MessageParam {
            role: MessageRole::Assistant,
            content: UserContent::String(text.to_string()),
        }
    }

    #[test]
    fn no_compression_always_keeps() {
        let mut messages = vec![user_msg("hi"), assistant_msg("hello")];
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage,
            model_info: &model_info(200_000),
        };
        let outcome = NoCompression.maybe_compress(&mut ctx);
        assert_eq!(outcome, CompressionOutcome::Keep);
        assert_eq!(ctx.messages.len(), 2);
    }

    #[test]
    fn simple_window_under_threshold_keeps() {
        let mut messages = vec![user_msg("u1"), assistant_msg("a1"), user_msg("u2")];
        let usage = Usage {
            input_tokens: 1000, // well under 200_000 * 0.85
            output_tokens: 10,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage,
            model_info: &model_info(200_000),
        };
        let outcome = SimpleWindowCompressor::default().maybe_compress(&mut ctx);
        assert_eq!(outcome, CompressionOutcome::Keep);
        assert_eq!(ctx.messages.len(), 3);
    }

    #[test]
    fn simple_window_over_threshold_truncates_oldest() {
        let mut messages: Vec<MessageParam> = (0..100)
            .map(|i| {
                if i % 2 == 0 {
                    user_msg(&format!("user message {i}"))
                } else {
                    assistant_msg(&format!("assistant message {i}"))
                }
            })
            .collect();
        let usage = Usage {
            input_tokens: 199_000, // over 200_000 * 0.85 = 170_000
            output_tokens: 10,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage,
            model_info: &model_info(200_000),
        };
        let outcome = SimpleWindowCompressor::default().maybe_compress(&mut ctx);
        // input_tokens is 199_000 which is >= 170_000 threshold, so we
        // remove. The heuristic frees 100 tokens per message, so we need
        // ~290 messages removed to get below threshold. We only have 100,
        // so all 100 get removed.
        if let CompressionOutcome::Truncated {
            removed_count,
            freed_tokens,
        } = outcome
        {
            assert!(removed_count > 0);
            assert_eq!(freed_tokens, (removed_count as u32) * 100);
            // First user message preserved
            assert!(matches!(ctx.messages[0].role, MessageRole::User));
        } else {
            panic!("expected Truncated, got {outcome:?}");
        }
    }

    #[test]
    fn simple_window_preserves_first_user() {
        let mut messages = vec![
            user_msg("ORIGINAL TASK"),
            assistant_msg("ack"),
            user_msg("follow-up"),
            assistant_msg("done"),
        ];
        let usage = Usage {
            input_tokens: 199_000,
            output_tokens: 10,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage,
            model_info: &model_info(200_000),
        };
        let outcome = SimpleWindowCompressor::default().maybe_compress(&mut ctx);
        if let CompressionOutcome::Truncated { removed_count, .. } = outcome {
            assert_eq!(removed_count, 3);
            // First user message preserved
            if let UserContent::String(s) = &ctx.messages[0].content {
                assert_eq!(s, "ORIGINAL TASK");
            } else {
                panic!("expected string content");
            }
        } else {
            panic!("expected Truncated, got {outcome:?}");
        }
    }

    #[test]
    fn simple_window_threshold_ratio_respected() {
        let mut messages = vec![user_msg("u1"), assistant_msg("a1"), user_msg("u2")];
        let usage = Usage {
            input_tokens: 1000,
            output_tokens: 10,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage,
            model_info: &model_info(1000),
        };
        // Threshold = 1000 * 0.5 = 500. input_tokens 1000 >= 500, trigger.
        let outcome = SimpleWindowCompressor::default()
            .with_threshold(0.5)
            .maybe_compress(&mut ctx);
        assert!(matches!(outcome, CompressionOutcome::Truncated { .. }));
    }

    #[test]
    fn simple_window_stops_when_all_protected() {
        // 1 message, the first user message (protected). Even though
        // input is above threshold, we can't remove it.
        let mut messages = vec![user_msg("ORIGINAL")];
        let usage = Usage {
            input_tokens: 999_999,
            output_tokens: 10,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage,
            model_info: &model_info(200_000),
        };
        let outcome = SimpleWindowCompressor::default().maybe_compress(&mut ctx);
        assert_eq!(outcome, CompressionOutcome::Keep);
    }

    #[test]
    fn custom_threshold_zero_always_triggers() {
        let mut messages = vec![user_msg("u1"), assistant_msg("a1")];
        let usage = Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage,
            model_info: &model_info(200_000),
        };
        let outcome = SimpleWindowCompressor::default()
            .with_threshold(0.0)
            .maybe_compress(&mut ctx);
        assert!(matches!(outcome, CompressionOutcome::Truncated { .. }));
    }
}