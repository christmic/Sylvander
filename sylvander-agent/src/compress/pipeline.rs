//! `CompressionPipeline` — ordered composition of compression
//! `layer`s. Cheap-first, expensive-last.
//!
//! The pipeline is the sole message-history compression path. Each `layer` is
//! independent, testable, and replaceable. A layer that
//! records a failure does NOT stop subsequent layers — pipeline
//! ordering is preserved and partial work is the norm.
//!
//! ## Construction
//!
//! ```ignore
//! use sylvander_agent::prelude::*;
//! use sylvander_agent::compress::layers::*;
//!
//! let pipeline = CompressionPipeline::builder()
//!     .layer(ToolResultBudgetLayer::new(disk.clone()))
//!     .layer(OrphanSnipLayer::new())
//!     .layer(MicroCompactLayer::new())
//!     .layer(ContextCollapseLayer::new())
//!     .build();
//! ```
//!
//! For a sensible default, use [`CompressionPipeline::default_for_model`]:
//!
//! ```ignore
//! let pipeline = CompressionPipeline::default_for_model(&model_info);
//! ```
//!
//! ## Driver dispatch
//!
//! The pipeline's `run_all` is async. The legacy `Compressor::maybe_compress`
//! is synchronous. The `CompressionDriver` enum in `loop_.rs` owns that
//! dispatch boundary.

use std::fmt;

use crate::compress::CompressContext;
use crate::compress::layer::{CompressionLayer, LayerReport};

/// Ordered list of compression layers. Cheap-first, expensive-last.
pub struct CompressionPipeline {
    layers: Vec<Box<dyn CompressionLayer>>,
}

impl fmt::Debug for CompressionPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompressionPipeline")
            .field(
                "layers",
                &self.layers.iter().map(|l| l.name()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl CompressionPipeline {
    /// Start a builder with no layers.
    #[must_use]
    pub fn builder() -> CompressionPipelineBuilder {
        CompressionPipelineBuilder::new()
    }

    /// Construct from a pre-built layer vec. Useful when layers
    /// come from dynamic sources (e.g. plugin loading).
    #[must_use]
    pub fn from_layers(layers: Vec<Box<dyn CompressionLayer>>) -> Self {
        Self { layers }
    }

    /// A sensible default pipeline for the given model: L1 + L2 + L3
    /// + L4. No L0 (disk dependency, opt-in).
    ///
    /// L4 is included because it has zero cost below the trigger
    /// threshold (returns no-op). When context fills up, it kicks
    /// in automatically using the `AgentLoop`'s client. L0 requires
    /// a `ToolResultDisk` so it's opt-in via custom pipeline.
    ///
    /// Order matters: L1 runs before L2 so orphan removal happens
    /// before in-place condensation; L3 runs last among the cheap
    /// `layer`s so it sees the messages after they've been cleaned;
    /// L4 runs last (the expensive semantic step).
    #[must_use]
    pub fn default_for_model(_model: &sylvander_llm_anthropic::api::model::ModelInfo) -> Self {
        Self::builder()
            .layer(crate::compress::layers::orphan_snip::OrphanSnipLayer::new())
            .layer(crate::compress::layers::micro_compact::MicroCompactLayer::new())
            .layer(crate::compress::layers::context_collapse::ContextCollapseLayer::new())
            .layer(crate::compress::layers::auto_compact::AutoCompactLayer::new())
            .build()
    }

    /// Number of layers in this pipeline.
    #[must_use]
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// True if the pipeline has no layers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Names of every layer, in execution order.
    #[must_use]
    pub fn layer_names(&self) -> Vec<&'static str> {
        self.layers.iter().map(|l| l.name()).collect()
    }

    /// Run all layers sequentially. Each layer's report is
    /// collected. A layer that records a failure does NOT stop
    /// subsequent layers.
    pub async fn run_all(&self, ctx: &mut CompressContext<'_>) -> Vec<LayerReport> {
        let mut reports = Vec::with_capacity(self.layers.len());
        for layer in &self.layers {
            let report = layer.apply(ctx).await;
            // Emit a tracing event for observability — the pipeline
            // doesn't decide what's important; the consumer does.
            if report.failure.is_some() {
                tracing::warn!(
                    layer = report.name,
                    reason = report.failure.as_deref().unwrap_or("?"),
                    "compression layer recorded failure"
                );
            } else if report.condensed_count > 0
                || report.removed_count > 0
                || report.freed_tokens > 0
            {
                tracing::debug!(
                    layer = report.name,
                    removed = report.removed_count,
                    condensed = report.condensed_count,
                    freed = report.freed_tokens,
                    "compression layer did work"
                );
            }
            reports.push(report);
        }
        reports
    }
}

/// Builder for [`CompressionPipeline`].
#[derive(Default)]
pub struct CompressionPipelineBuilder {
    layers: Vec<Box<dyn CompressionLayer>>,
}

impl CompressionPipelineBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a layer. Order = call order.
    pub fn layer<L: CompressionLayer + 'static>(mut self, layer: L) -> Self {
        self.layers.push(Box::new(layer));
        self
    }

    /// Append a boxed layer (for dynamic composition).
    pub fn layer_boxed(mut self, layer: Box<dyn CompressionLayer>) -> Self {
        self.layers.push(layer);
        self
    }

    /// Finalize the pipeline. Order of `layer(...)` calls = order
    /// of execution.
    #[must_use]
    pub fn build(self) -> CompressionPipeline {
        CompressionPipeline {
            layers: self.layers,
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/compress_pipeline.rs"]
mod tests;
