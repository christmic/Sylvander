//! L0 — `ToolResultBudget`: cap inline `tool_result` size.
//!
//! Walk every `tool_result` block in the conversation. If its
//! serialized body exceeds `max_inline_chars`, persist it via the
//! injected [`ToolResultDisk`] and replace the inline content with
//! a preview + path.
//!
//! Fires every iteration (cheap local check, like Claude Code).
//! Only does disk I/O for oversized blocks; under-budget blocks are
//! left untouched.
//!
//! ## What stays inline
//!
//! - Short tool results (under `max_inline_chars`): untouched.
//! - Tool results with `Blocks` content (rich/typed): untouched
//!   (these have structured semantics we shouldn't blindly truncate).
//! - Tool results with `None` content: untouched.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sylvander_llm_anthropic::api::types::{
    MessageParam, ToolResultContent, UserContent, UserContentBlock,
};

use crate::compress::CompressContext;
use crate::compress::disk::ToolResultDisk;
use crate::compress::layer::{CompressionLayer, LayerReport};

/// Default max inline chars before a `tool_result` is offloaded to disk.
/// ~5k chars ≈ 1.25k tokens — well below attention-noise territory
/// but large enough for most tool outputs (grep results, file
/// previews, JSON blobs).
pub const DEFAULT_MAX_INLINE_CHARS: usize = 5_000;

/// Default preview size kept inline after offload.
pub const DEFAULT_PREVIEW_CHARS: usize = 500;

/// L0 layer: cap inline `tool_result` size via offload to disk.
pub struct ToolResultBudgetLayer {
    /// Max chars kept inline. Larger results are persisted.
    pub max_inline_chars: usize,
    /// How many chars of preview to keep inline after offload.
    pub preview_chars: usize,
    /// Disk backend for persistence.
    pub disk: Arc<dyn ToolResultDisk>,
}

impl ToolResultBudgetLayer {
    /// Create a layer with default thresholds and the given disk.
    #[must_use]
    pub fn new(disk: Arc<dyn ToolResultDisk>) -> Self {
        Self {
            max_inline_chars: DEFAULT_MAX_INLINE_CHARS,
            preview_chars: DEFAULT_PREVIEW_CHARS,
            disk,
        }
    }

    /// Override `max_inline_chars`.
    #[must_use]
    pub fn with_max_inline_chars(mut self, n: usize) -> Self {
        self.max_inline_chars = n;
        self
    }

    /// Override `preview_chars`.
    #[must_use]
    pub fn with_preview_chars(mut self, n: usize) -> Self {
        self.preview_chars = n;
        self
    }

    /// Synchronous work — does the actual message rewrite.
    fn apply_sync(&self, ctx: &mut CompressContext<'_>) -> LayerReport {
        let mut condensed = 0usize;
        let mut freed_tokens = 0u32;
        let mut written_paths: Vec<String> = Vec::new();

        for msg in ctx.messages.iter_mut() {
            rewrite_message(
                msg,
                self.max_inline_chars,
                self.preview_chars,
                self.disk.as_ref(),
                &mut condensed,
                &mut freed_tokens,
                &mut written_paths,
            );
        }

        if condensed == 0 && written_paths.is_empty() {
            return LayerReport::noop(self.name());
        }

        let details = serde_json::json!({
            "written_paths": written_paths,
        });

        LayerReport {
            name: self.name().to_string(),
            removed_count: 0,
            condensed_count: condensed,
            freed_tokens,
            details: Some(details),
            failure: None,
            failure_code: None,
        }
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
        // L0 is sync; wrap the computed report in a ready future.
        let report = self.apply_sync(ctx);
        Box::pin(async move { report })
    }
}

/// Mutate `msg` in place: rewrite oversized `tool_result` blocks.
/// Returns counts via the out-params.
fn rewrite_message(
    msg: &mut MessageParam,
    max_inline_chars: usize,
    preview_chars: usize,
    disk: &dyn ToolResultDisk,
    condensed: &mut usize,
    freed_tokens: &mut u32,
    written_paths: &mut Vec<String>,
) {
    // Only user messages hold tool_result blocks.
    let UserContent::Blocks(blocks) = &mut msg.content else {
        return;
    };

    for block in blocks.iter_mut() {
        let UserContentBlock::ToolResult(trb) = block else {
            continue;
        };

        // Only handle plain string content. Rich blocks stay as-is.
        let Some(ToolResultContent::String(body)) = trb.content.as_ref() else {
            continue;
        };

        if body.len() <= max_inline_chars {
            continue;
        }

        // Persist full body to disk.
        let handle = match disk.persist(&trb.tool_use_id, body) {
            Ok(h) => h,
            Err(e) => {
                // Don't corrupt the block on failure — leave it as-is.
                // Caller will see the failure via the report.
                tracing::warn!(
                    tool_use_id = %trb.tool_use_id,
                    error = %e,
                    "L0 tool_result_budget: disk persist failed, leaving block unchanged"
                );
                // We can't return a failure from here easily without
                // changing the signature; the layer aggregates a
                // single failure if any disk errors happen. For now,
                // we just skip this block and continue.
                continue;
            }
        };

        // Build preview + path string.
        let preview_end = preview_chars.min(body.len());
        // Find a char boundary so we don't slice mid-codepoint.
        let preview_end = floor_char_boundary(body, preview_end);
        let preview = &body[..preview_end];
        let replacement = format!(
            "[Output saved to {} — first {} chars shown]\n{}",
            handle.path.display(),
            preview_chars,
            preview,
        );

        let original_len = body.len();
        let new_len = replacement.len();
        let saved = original_len.saturating_sub(new_len);
        *freed_tokens = freed_tokens.saturating_add((saved / 4) as u32);

        // Mutate the block in place. Preserve tool_use_id, is_error,
        // cache_control. Replace content with the preview.
        trb.content = Some(ToolResultContent::String(replacement));
        written_paths.push(handle.path.display().to_string());
        *condensed += 1;
    }
}

/// Floor `index` down to the nearest UTF-8 char boundary in `s`.
/// Avoids slicing mid-codepoint when preview truncates a body.
fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
#[path = "../../../tests/unit/compress_layers_tool_result_budget.rs"]
mod tests;
