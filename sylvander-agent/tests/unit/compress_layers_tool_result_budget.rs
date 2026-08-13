use async_trait::async_trait;
use sylvander_llm_core::{
    ChatMessage, ContentBlock, ModelCapabilities, ModelInfo, ModelRef, TokenUsage,
    ToolResultContent,
};

use super::*;
use crate::artifact::{ArtifactReference, ArtifactStoreError, ArtifactWrite, TurnArtifactStore};
use crate::test_support::InMemoryArtifactStore;

struct UnavailableStore;

#[async_trait]
impl TurnArtifactStore for UnavailableStore {
    async fn persist(
        &self,
        _artifact: ArtifactWrite,
    ) -> Result<ArtifactReference, ArtifactStoreError> {
        Err(ArtifactStoreError::Unavailable)
    }
}

fn model(provider: &str, name: &str) -> ModelInfo {
    ModelInfo {
        reference: ModelRef::new(provider, name),
        context_window: 200_000,
        max_output_tokens: 8_192,
        capabilities: ModelCapabilities::empty(),
    }
}

fn user_result(call_id: &str, body: &str) -> ChatMessage {
    ChatMessage::user_blocks(vec![ContentBlock::tool_result_text(call_id, body, false)])
}

fn text_body(message: &ChatMessage) -> &str {
    let ContentBlock::ToolResult { content, .. } = &message.content[0] else {
        panic!("expected tool result");
    };
    let ToolResultContent::Text { text } = &content[0] else {
        panic!("expected text result");
    };
    text
}

#[tokio::test]
async fn missing_store_is_noop_and_preserves_only_copy() {
    let layer = ToolResultBudgetLayer::new().with_max_inline_chars(5);
    let original = "oversized";
    let mut messages = vec![user_result("call", original)];
    let usage = TokenUsage::default();
    let model = model("test", "test");
    let mut context = CompressContext::new(&mut messages, &usage, &model);

    let report = layer.apply(&mut context).await;

    assert_eq!(report, LayerReport::noop("tool_result_budget"));
    assert_eq!(text_body(&messages[0]), original);
}

#[tokio::test]
async fn retains_content_and_exposes_only_opaque_locator() {
    let store = InMemoryArtifactStore::new();
    let layer = ToolResultBudgetLayer::new()
        .with_max_inline_chars(50)
        .with_preview_chars(20);
    let original = "x".repeat(200);
    let mut messages = vec![user_result("call-1", &original)];
    let usage = TokenUsage::default();
    let model = model("anthropic", "claude-sonnet");
    let mut context =
        CompressContext::new(&mut messages, &usage, &model).with_artifact_store(&store);

    let report = layer.apply(&mut context).await;

    assert_eq!(report.condensed_count, 1);
    assert!(report.freed_tokens > 0);
    assert_eq!(store.get("call-1").as_deref(), Some(original.as_str()));
    let rewritten = text_body(&messages[0]);
    assert!(rewritten.contains("artifact:call-1"));
    assert!(!rewritten.contains('/') && !rewritten.contains("in-memory"));
    assert_eq!(
        report.details,
        Some(serde_json::json!({"artifact_locators": ["artifact:call-1"]}))
    );
}

#[tokio::test]
async fn persistence_failure_keeps_original_and_reports_stable_class() {
    let store = UnavailableStore;
    let layer = ToolResultBudgetLayer::new().with_max_inline_chars(5);
    let original = "must remain available";
    let mut messages = vec![user_result("call", original)];
    let usage = TokenUsage::default();
    let model = model("openai", "gpt-5");
    let mut context =
        CompressContext::new(&mut messages, &usage, &model).with_artifact_store(&store);

    let report = layer.apply(&mut context).await;

    assert_eq!(text_body(&messages[0]), original);
    assert_eq!(report.condensed_count, 0);
    assert_eq!(
        report.failure_code,
        Some(CompactionFailureCode::Persistence)
    );
}

#[tokio::test]
async fn preview_respects_utf8_boundary_and_preserves_error_flag() {
    let store = InMemoryArtifactStore::new();
    let layer = ToolResultBudgetLayer::new()
        .with_max_inline_chars(3)
        .with_preview_chars(5);
    let mut messages = vec![ChatMessage::user_blocks(vec![
        ContentBlock::tool_result_text("unicode", "你好世界", true),
    ])];
    let usage = TokenUsage::default();
    let model = model("dashscope", "qwen3-max");
    let mut context =
        CompressContext::new(&mut messages, &usage, &model).with_artifact_store(&store);

    let report = layer.apply(&mut context).await;

    assert_eq!(report.condensed_count, 1);
    assert!(text_body(&messages[0]).ends_with('你'));
    let ContentBlock::ToolResult {
        call_id, is_error, ..
    } = &messages[0].content[0]
    else {
        panic!("expected tool result");
    };
    assert_eq!(call_id, "unicode");
    assert!(*is_error);
}

#[tokio::test]
async fn policy_is_provider_and_model_independent() {
    let cases = [
        ("anthropic", "claude-opus"),
        ("openai", "gpt-5"),
        ("dashscope", "qwen3-max"),
    ];

    for (provider, name) in cases {
        let store = InMemoryArtifactStore::new();
        let layer = ToolResultBudgetLayer::new().with_max_inline_chars(4);
        let mut messages = vec![user_result("same-call", "same-result")];
        let usage = TokenUsage::default();
        let model = model(provider, name);
        let mut context =
            CompressContext::new(&mut messages, &usage, &model).with_artifact_store(&store);

        let report = layer.apply(&mut context).await;

        assert_eq!(report.condensed_count, 1, "{provider}/{name}");
        assert_eq!(store.get("same-call").as_deref(), Some("same-result"));
    }
}
