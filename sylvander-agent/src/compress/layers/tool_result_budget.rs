//! L0 — bound plain-text tool results retained by Runtime.
//!
//! Oversized text is persisted through the turn-bound artifact port before it
//! is replaced with a preview. Rich blocks are unchanged because serializing
//! them here would discard provider-neutral semantics. Missing or failed
//! storage leaves the original content intact.

use std::future::Future;
use std::pin::Pin;

use sylvander_llm_core::{ContentBlock, ToolResultContent};

use crate::artifact::{ArtifactStoreError, ArtifactWrite};
use crate::compress::CompressContext;
use crate::compress::error::{CompactionError, CompactionFailureCode};
use crate::compress::layer::{CompressionLayer, LayerReport};

/// Default maximum inline characters before plain text is retained externally.
pub const DEFAULT_MAX_INLINE_CHARS: usize = 5_000;
/// Default preview characters preserved in model context.
pub const DEFAULT_PREVIEW_CHARS: usize = 500;
const TEXT_MEDIA_TYPE: &str = "text/plain; charset=utf-8";

/// Cheap first compression layer for oversized plain-text tool results.
#[derive(Clone, Copy, Debug)]
pub struct ToolResultBudgetLayer {
    /// Maximum number of inline UTF-8 bytes accepted without retention.
    pub max_inline_chars: usize,
    /// Maximum preview boundary, rounded down to a valid UTF-8 boundary.
    pub preview_chars: usize,
}

impl ToolResultBudgetLayer {
    /// Create the location-neutral budget policy.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_inline_chars: DEFAULT_MAX_INLINE_CHARS,
            preview_chars: DEFAULT_PREVIEW_CHARS,
        }
    }

    /// Override the inline threshold.
    #[must_use]
    pub const fn with_max_inline_chars(mut self, value: usize) -> Self {
        self.max_inline_chars = value;
        self
    }

    /// Override the preview threshold.
    #[must_use]
    pub const fn with_preview_chars(mut self, value: usize) -> Self {
        self.preview_chars = value;
        self
    }

    async fn apply_async(&self, ctx: &mut CompressContext<'_>) -> LayerReport {
        let Some(store) = ctx.artifact_store else {
            return LayerReport::noop(self.name());
        };
        let mut condensed = 0usize;
        let mut freed_tokens = 0u32;
        let mut locators = Vec::new();
        let mut failed = false;

        for message_index in 0..ctx.messages.len() {
            for block_index in 0..ctx.messages[message_index].content.len() {
                let candidate = candidate(
                    &ctx.messages[message_index].content[block_index],
                    self.max_inline_chars,
                );
                let Some((call_id, body)) = candidate else {
                    continue;
                };
                let write = ArtifactWrite {
                    call_id,
                    media_type: TEXT_MEDIA_TYPE.to_string(),
                    payload: body.as_bytes().to_vec(),
                };
                let reference = match store.persist(write).await {
                    Ok(reference) => reference,
                    Err(error) => {
                        failed = true;
                        record_failure(error);
                        continue;
                    }
                };
                let preview_end = floor_char_boundary(&body, self.preview_chars.min(body.len()));
                let replacement = format!(
                    "[Output retained as {} and truncated; first {} bytes shown]\n{}",
                    reference.locator,
                    preview_end,
                    &body[..preview_end],
                );
                let saved = body.len().saturating_sub(replacement.len());
                freed_tokens = freed_tokens.saturating_add((saved / 4) as u32);
                let ContentBlock::ToolResult { content, .. } =
                    &mut ctx.messages[message_index].content[block_index]
                else {
                    unreachable!("candidate only returns tool results");
                };
                *content = vec![ToolResultContent::Text { text: replacement }];
                locators.push(reference.locator);
                condensed += 1;
            }
        }

        let mut report = LayerReport {
            name: self.name().to_string(),
            removed_count: 0,
            condensed_count: condensed,
            freed_tokens,
            details: Some(serde_json::json!({ "artifact_locators": locators })),
            failure: None,
            failure_code: None,
        };
        if failed {
            let error = CompactionError::new(CompactionFailureCode::Persistence);
            report.failure = Some(error.compatibility_reason().to_string());
            report.failure_code = Some(error.code);
        }
        report
    }
}

impl Default for ToolResultBudgetLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl CompressionLayer for ToolResultBudgetLayer {
    fn name(&self) -> &'static str {
        "tool_result_budget"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut CompressContext<'_>,
    ) -> Pin<Box<dyn Future<Output = LayerReport> + Send + 'a>> {
        Box::pin(self.apply_async(ctx))
    }
}

fn candidate(block: &ContentBlock, max_inline_chars: usize) -> Option<(String, String)> {
    let ContentBlock::ToolResult {
        call_id, content, ..
    } = block
    else {
        return None;
    };
    let [ToolResultContent::Text { text }] = content.as_slice() else {
        return None;
    };
    (text.len() > max_inline_chars).then(|| (call_id.clone(), text.clone()))
}

fn record_failure(error: ArtifactStoreError) {
    tracing::warn!(error = %error, "artifact retention failed; preserving inline tool result");
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    if index >= value.len() {
        return value.len();
    }
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
#[path = "../../../tests/unit/compress_layers_tool_result_budget.rs"]
mod tests;
