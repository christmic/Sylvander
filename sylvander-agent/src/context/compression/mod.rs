//! Compression pipeline for the agent loop's message history.
//!
//! A multi-layer [`CompressionPipeline`]
//! runs cheap-to-expensive layers in sequence. It is the only compression
//! path; there is no legacy single-strategy fallback.
//!
//! Layers available:
//! - L0: [`ToolResultBudgetLayer`](crate::context::compression::layers::tool_result_budget::ToolResultBudgetLayer)
//!   — cap inline `tool_result` size via Runtime-owned artifact retention
//! - L1: [`OrphanSnipLayer`](crate::context::compression::layers::orphan_snip::OrphanSnipLayer)
//!   — drop `tool_result` blocks with no matching `tool_use`
//! - L2: [`MicroCompactLayer`](crate::context::compression::layers::micro_compact::MicroCompactLayer)
//!   — replace old `tool_result`s with placeholders
//! - L3: [`ContextCollapseLayer`](crate::context::compression::layers::context_collapse::ContextCollapseLayer)
//!   — trim old thinking blocks
//! - L4: [`AutoCompactLayer`](crate::context::compression::layers::auto_compact::AutoCompactLayer)
//!   — LLM-driven summarization when context budget is exhausted

pub mod auto_compact_llm;
pub mod error;
pub mod layer;
pub mod layers;
pub mod pipeline;

pub use auto_compact_llm::{AutoCompactLlm, DEFAULT_SUMMARY_PROMPT};

use sylvander_llm_core::{ChatMessage, ModelInfo, TokenUsage};

use crate::context::compression::pipeline::CompressionPipeline;
use crate::execution::artifact::TurnArtifactStore;

/// Context passed to each layer in a pipeline.
///
/// Layers mutate `messages` (the model-visible history) and report
/// what they did via a [`LayerReport`](crate::context::compression::layer::LayerReport).
pub struct CompressContext<'a> {
    /// Mutable message history. Layers may drop from the front or
    /// rewrite inner blocks in place.
    pub messages: &'a mut Vec<ChatMessage>,
    /// Token usage reported by the last LLM response.
    pub last_usage: &'a TokenUsage,
    /// Resolved model metadata (for `context_window` + capabilities).
    pub model_info: &'a ModelInfo,
    /// Optional LLM for L4 (auto-compact). Populated by
    /// `AgentLoop`; `None` in unit tests where L4 should be a no-op.
    pub auto_compact_llm: Option<&'a dyn AutoCompactLlm>,
    /// Optional Runtime authority for retaining oversized content. A missing
    /// port makes artifact-dependent layers fail open without discarding data.
    pub artifact_store: Option<&'a dyn TurnArtifactStore>,
}

impl<'a> CompressContext<'a> {
    /// Construct a context with the standard 3 fields. The LLM is
    /// `None` by default — use [`Self::with_auto_compact_llm`] to
    /// set it.
    #[must_use]
    pub fn new(
        messages: &'a mut Vec<ChatMessage>,
        last_usage: &'a TokenUsage,
        model_info: &'a ModelInfo,
    ) -> Self {
        Self {
            messages,
            last_usage,
            model_info,
            auto_compact_llm: None,
            artifact_store: None,
        }
    }

    /// Attach an LLM for L4.
    #[must_use]
    pub fn with_auto_compact_llm(mut self, llm: &'a dyn AutoCompactLlm) -> Self {
        self.auto_compact_llm = Some(llm);
        self
    }

    /// Attach the artifact authority already bound to this exact turn.
    #[must_use]
    pub fn with_artifact_store(mut self, store: &'a dyn TurnArtifactStore) -> Self {
        self.artifact_store = Some(store);
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
