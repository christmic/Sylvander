use super::*;
use sylvander_llm_core::{ChatMessage, ModelCapabilities, ModelInfo, ModelRef, TokenUsage};

/// A trivial layer that always returns a no-op report. Used to
/// verify the trait is object-safe and dyn-compatible.
struct NoopLayer;

impl CompressionLayer for NoopLayer {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut CompressContext<'_>,
    ) -> Pin<Box<dyn Future<Output = LayerReport> + Send + 'a>> {
        Box::pin(async { LayerReport::noop(self.name()) })
    }
}

#[tokio::test]
async fn trait_is_object_safe_and_dispatchable() {
    let layer: Box<dyn CompressionLayer> = Box::new(NoopLayer);
    assert_eq!(layer.name(), "noop");

    // Build a minimal context with an empty messages vec.
    let mut messages: Vec<ChatMessage> = vec![];
    let usage = TokenUsage::default();
    let model = ModelInfo {
        reference: ModelRef::new("test", "test"),
        context_window: 200_000,
        max_output_tokens: 8192,
        capabilities: ModelCapabilities::empty(),
    };
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage,
        model_info: &model,
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.name, "noop");
    assert_eq!(report.removed_count, 0);
    assert_eq!(report.condensed_count, 0);
    assert_eq!(report.failure, None);
}

#[test]
fn layer_report_helpers_aggregate() {
    let layers = vec![
        LayerReport {
            name: "a".into(),
            removed_count: 2,
            condensed_count: 1,
            freed_tokens: 100,
            details: None,
            failure: None,
            failure_code: None,
        },
        LayerReport {
            name: "b".into(),
            removed_count: 0,
            condensed_count: 3,
            freed_tokens: 50,
            details: None,
            failure: None,
            failure_code: None,
        },
        LayerReport {
            name: "c".into(),
            removed_count: 0,
            condensed_count: 0,
            freed_tokens: 0,
            details: None,
            failure: Some("boom".into()),
            failure_code: Some(CompactionFailureCode::Other),
        },
    ];
    assert_eq!(total_removed(&layers), 2);
    assert_eq!(total_condensed(&layers), 4);
    assert_eq!(total_freed(&layers), 150);
    assert_eq!(first_failure(&layers), Some("boom"));
    assert_eq!(
        first_failure_error(&layers).unwrap().code,
        CompactionFailureCode::Other
    );
}
