//! L3 — `ContextCollapse`: projected/stub view of inactive context.
//!
//! **Stub** — real implementation is M4+. The slot is reserved so
//! the pipeline architecture is stable: dropping in a real impl
//! later requires no API changes.
//!
//! In production, this layer would fold unused subtrees of a
//! project (unused files, dropped subagents, dormant skill bodies)
//! into stubs the model can ignore. The model can ask to "expand"
//! any stub when it becomes relevant again.
//!
//! Today, this layer always returns a failure report, so the
//! pipeline still runs end-to-end but the layer itself is a no-op.

use std::future::Future;
use std::pin::Pin;

use crate::compress::layer::{CompressionLayer, LayerReport};
use crate::compress::CompressContext;

/// L3 stub. Reserved slot for the future `ContextCollapse` impl.
#[derive(Debug, Default, Clone, Copy)]
pub struct ContextCollapseLayer;

impl ContextCollapseLayer {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl CompressionLayer for ContextCollapseLayer {
    fn name(&self) -> &'static str {
        "context_collapse"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut CompressContext<'_>,
    ) -> Pin<Box<dyn Future<Output = LayerReport> + Send + 'a>> {
        Box::pin(async move {
            LayerReport::failed(self.name(), "not implemented (planned for M4+)")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylvander_llm_anthropic::api::model::ModelInfo;
    use sylvander_llm_anthropic::api::types::Usage;

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

    #[tokio::test]
    async fn stub_returns_failure() {
        let layer = ContextCollapseLayer::new();
        let mut messages: Vec<sylvander_llm_anthropic::api::types::MessageParam> = vec![];
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage(),
            model_info: &model(),
        };

        let report = layer.apply(&mut ctx).await;
        assert_eq!(report.name, "context_collapse");
        assert!(report.failure.is_some());
        assert!(report
            .failure
            .as_deref()
            .unwrap()
            .contains("not implemented"));
        // No actual work done.
        assert_eq!(report.condensed_count, 0);
        assert_eq!(report.removed_count, 0);
    }
}