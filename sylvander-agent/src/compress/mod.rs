//! Compression pipeline for the agent loop's message history.
//!
//! M3 ships a multi-layer [`CompressionPipeline`] (see
//! `pipeline.rs` and `layers/`) that runs cheap-to-expensive layers
//! in sequence. The pipeline is the only compression path — there's
//! no legacy single-strategy fallback.
//!
//! Layers available:
//! - L0: [`ToolResultBudgetLayer`](self::layers::tool_result_budget::ToolResultBudgetLayer)
//!   — cap inline `tool_result` size via disk offload
//! - L1: [`OrphanSnipLayer`](self::layers::orphan_snip::OrphanSnipLayer)
//!   — drop `tool_result` blocks with no matching `tool_use`
//! - L2: [`MicroCompactLayer`](self::layers::micro_compact::MicroCompactLayer)
//!   — replace old `tool_result`s with placeholders
//! - L3: [`ContextCollapseLayer`](self::layers::context_collapse::ContextCollapseLayer)
//!   — trim old thinking blocks
//! - L4: [`AutoCompactLayer`](self::layers::auto_compact::AutoCompactLayer)
//!   — LLM-driven summarization when context budget is exhausted

pub mod auto_compact_llm;
pub mod disk;
pub mod layer;
pub mod layers;
pub mod pipeline;

pub use auto_compact_llm::{
    AgentLoopAutoCompactLlm, AutoCompactLlm, DEFAULT_SUMMARY_PROMPT,
};

use sylvander_llm_anthropic::api::model::ModelInfo;
use sylvander_llm_anthropic::api::types::Usage;

use crate::compress::pipeline::CompressionPipeline;

/// Context passed to each layer in a pipeline.
///
/// Layers mutate `messages` (the model-visible history) and report
/// what they did via a [`LayerReport`](self::layer::LayerReport).
pub struct CompressContext<'a> {
    /// Mutable message history. Layers may drop from the front or
    /// rewrite inner blocks in place.
    pub messages: &'a mut Vec<sylvander_llm_anthropic::api::types::MessageParam>,
    /// Token usage reported by the last LLM response.
    pub last_usage: &'a Usage,
    /// Resolved model metadata (for `context_window` + capabilities).
    pub model_info: &'a ModelInfo,
    /// Optional LLM for L4 (auto-compact). Populated by
    /// `AgentLoop`; `None` in unit tests where L4 should be a no-op.
    pub auto_compact_llm: Option<&'a dyn AutoCompactLlm>,
}

impl<'a> CompressContext<'a> {
    /// Construct a context with the standard 3 fields. The LLM is
    /// `None` by default — use [`Self::with_auto_compact_llm`] to
    /// set it.
    #[must_use]
    pub fn new(
        messages: &'a mut Vec<sylvander_llm_anthropic::api::types::MessageParam>,
        last_usage: &'a Usage,
        model_info: &'a ModelInfo,
    ) -> Self {
        Self {
            messages,
            last_usage,
            model_info,
            auto_compact_llm: None,
        }
    }

    /// Attach an LLM for L4.
    #[must_use]
    pub fn with_auto_compact_llm(mut self, llm: &'a dyn AutoCompactLlm) -> Self {
        self.auto_compact_llm = Some(llm);
        self
    }
}

/// Run a compression pipeline against a [`CompressContext`]. Convenience
/// wrapper around `pipeline.run_all(&mut ctx).await` that keeps the
/// import surface tight for callers that don't want to import
/// `CompressionPipeline` directly.
pub async fn run_pipeline(
    pipeline: &CompressionPipeline,
    ctx: &mut CompressContext<'_>,
) -> Vec<self::layer::LayerReport> {
    pipeline.run_all(ctx).await
}