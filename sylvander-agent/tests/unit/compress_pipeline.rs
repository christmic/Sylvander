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
async fn default_for_model_contains_l1_l2_l3_l4() {
    let model = model();
    let pipeline = CompressionPipeline::default_for_model(&model);
    let names = pipeline.layer_names();
    assert_eq!(
        names,
        vec![
            "orphan_snip",
            "micro_compact",
            "context_collapse",
            "auto_compact"
        ]
    );
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
        content: UserContent::Blocks(vec![UserContentBlock::ToolResult(ToolResultBlock::new(
            "orphan",
            "stale result",
        ))]),
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
