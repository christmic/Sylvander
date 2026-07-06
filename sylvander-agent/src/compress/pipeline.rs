//! `CompressionPipeline` — ordered composition of compression
//! layers. Cheap-first, expensive-last.
//!
//! The pipeline is the primary way to use compression in M3. Each
//! layer is independent, testable, and replaceable. A layer that
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
//!     .layer(ContextCollapseLayer::new()) // L3 stub
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
//! is sync. The `CompressionDriver` enum (in `loop_.rs`) decides
//! which path to take — see commit 8.

use std::fmt;

use crate::compress::layer::{CompressionLayer, LayerReport};
use crate::compress::CompressContext;

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

    /// A sensible default pipeline for the given model: L1 +
    /// L2 (no L0 disk dependency, no L3 stub, no L4 LLM cost).
    /// Order matters: L1 runs before L2 so orphan removal happens
    /// before in-place condensation.
    ///
    /// L0 (disk offload) is opt-in because it requires a
    /// `ToolResultDisk`. L4 (LLM summary) is opt-in because it
    /// requires an `AutoCompactLlm` and has cost.
    #[must_use]
    pub fn default_for_model(_model: &sylvander_llm_anthropic::api::model::ModelInfo) -> Self {
        Self::builder()
            .layer(crate::compress::layers::orphan_snip::OrphanSnipLayer::new())
            .layer(crate::compress::layers::micro_compact::MicroCompactLayer::new())
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
mod tests {
    use super::*;
    use crate::compress::layer::CompressionLayer;
    use crate::compress::layers::context_collapse::ContextCollapseLayer;
    use crate::compress::layers::orphan_snip::OrphanSnipLayer;
    use std::future::Future;
    use std::pin::Pin;
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

    /// Test layer that records its index in execution order.
    struct IndexedLayer {
        idx: usize,
        log: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
    }

    impl CompressionLayer for IndexedLayer {
        fn name(&self) -> &'static str {
            "indexed"
        }
        fn apply<'a>(
            &'a self,
            _ctx: &'a mut CompressContext<'_>,
        ) -> Pin<Box<dyn Future<Output = LayerReport> + Send + 'a>> {
            let log = self.log.clone();
            let idx = self.idx;
            Box::pin(async move {
                log.lock().unwrap().push(idx);
                LayerReport::noop("indexed")
            })
        }
    }

    /// Test layer that always fails.
    struct FailingLayer;

    impl CompressionLayer for FailingLayer {
        fn name(&self) -> &'static str {
            "failing"
        }
        fn apply<'a>(
            &'a self,
            _ctx: &'a mut CompressContext<'_>,
        ) -> Pin<Box<dyn Future<Output = LayerReport> + Send + 'a>> {
            Box::pin(async move { LayerReport::failed("failing", "synthetic test failure") })
        }
    }

    #[tokio::test]
    async fn empty_pipeline_runs_no_layers() {
        let pipeline = CompressionPipeline::builder().build();
        let mut messages: Vec<MessageParam> = vec![];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };

        let reports = pipeline.run_all(&mut ctx).await;
        assert!(reports.is_empty());
        assert_eq!(pipeline.len(), 0);
        assert!(pipeline.is_empty());
    }

    #[tokio::test]
    async fn layers_run_in_order() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let pipeline = CompressionPipeline::builder()
            .layer(IndexedLayer {
                idx: 0,
                log: log.clone(),
            })
            .layer(IndexedLayer {
                idx: 1,
                log: log.clone(),
            })
            .layer(IndexedLayer {
                idx: 2,
                log: log.clone(),
            })
            .build();

        let mut messages: Vec<MessageParam> = vec![];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };
        let _ = pipeline.run_all(&mut ctx).await;
        assert_eq!(*log.lock().unwrap(), vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn failure_in_one_layer_does_not_stop_others() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let pipeline = CompressionPipeline::builder()
            .layer(IndexedLayer {
                idx: 0,
                log: log.clone(),
            })
            .layer(FailingLayer)
            .layer(IndexedLayer {
                idx: 2,
                log: log.clone(),
            })
            .build();

        let mut messages: Vec<MessageParam> = vec![];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };
        let reports = pipeline.run_all(&mut ctx).await;

        assert_eq!(reports.len(), 3);
        assert!(reports[1].failure.is_some());
        // Both indexed layers still ran (0 and 2).
        assert_eq!(*log.lock().unwrap(), vec![0, 2]);
    }

    #[tokio::test]
    async fn default_for_model_contains_l1_and_l2() {
        let model = model();
        let pipeline = CompressionPipeline::default_for_model(&model);
        let names = pipeline.layer_names();
        assert_eq!(names, vec!["orphan_snip", "micro_compact"]);
    }

    #[tokio::test]
    async fn default_for_model_actually_drops_orphans() {
        // Sanity: the default pipeline actually does its job on a
        // synthetic conversation with an orphan tool_result.
        use sylvander_llm_anthropic::api::types::{
            MessageRole, ToolResultBlock, UserContent, UserContentBlock,
        };
        let pipeline = CompressionPipeline::default_for_model(&model());

        let mut messages = vec![MessageParam {
            role: MessageRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::ToolResult(
                ToolResultBlock::new("orphan", "stale result"),
            )]),
        }];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
            auto_compact_llm: None,
        };
        let reports = pipeline.run_all(&mut ctx).await;
        // L1 dropped the orphan.
        assert!(reports.iter().any(|r| r.condensed_count == 1));
        let UserContent::Blocks(blocks) = &messages[0].content else {
            panic!("expected blocks");
        };
        assert!(blocks.is_empty());
    }

    #[test]
    fn builder_layer_names_in_order() {
        let pipeline = CompressionPipeline::builder()
            .layer(OrphanSnipLayer::new())
            .layer(ContextCollapseLayer::new())
            .layer(OrphanSnipLayer::new()) // intentional dup to test ordering
            .build();

        assert_eq!(
            pipeline.layer_names(),
            vec!["orphan_snip", "context_collapse", "orphan_snip"]
        );
        assert_eq!(pipeline.len(), 3);
        assert!(!pipeline.is_empty());
    }
}