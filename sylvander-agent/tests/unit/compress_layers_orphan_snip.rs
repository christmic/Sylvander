use super::*;
use sylvander_llm_core::{
    ChatMessage, ContentBlock, ModelCapabilities, ModelInfo, ModelRef, TokenUsage,
};

fn model() -> ModelInfo {
    ModelInfo {
        reference: ModelRef::new("test", "test"),
        context_window: 200_000,
        max_output_tokens: 8192,
        capabilities: ModelCapabilities::empty(),
    }
}

fn usage() -> TokenUsage {
    TokenUsage::default()
}

fn user_with_tool_result(tool_use_id: &str) -> ChatMessage {
    ChatMessage::user_blocks(vec![ContentBlock::tool_result_text(
        tool_use_id,
        "result",
        false,
    )])
}

fn assistant_with_tool_use(tool_use_id: &str) -> ChatMessage {
    ChatMessage::assistant(vec![ContentBlock::ToolCall {
        id: tool_use_id.into(),
        name: "fake_tool".into(),
        arguments: serde_json::json!({}),
    }])
}

#[tokio::test]
async fn removes_tool_result_with_no_matching_tool_use() {
    let layer = OrphanSnipLayer::new();
    let mut messages = vec![user_with_tool_result("orphan_id")];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
        artifact_store: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 1);
    // Block was removed; message is now empty.
    assert!(messages[0].content.is_empty());
}

#[tokio::test]
async fn keeps_tool_result_with_matching_tool_use() {
    let layer = OrphanSnipLayer::new();
    let mut messages = vec![
        assistant_with_tool_use("paired_id"),
        user_with_tool_result("paired_id"),
    ];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
        artifact_store: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
    assert_eq!(messages[1].content.len(), 1);
}

#[tokio::test]
async fn removes_multiple_orphans_in_one_pass() {
    let layer = OrphanSnipLayer::new();
    let mut messages = vec![
        user_with_tool_result("orphan_1"),
        user_with_tool_result("paired"),
        assistant_with_tool_use("paired"),
        user_with_tool_result("orphan_2"),
    ];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
        artifact_store: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 2);
    // The paired one remains.
    assert_eq!(messages[1].content.len(), 1);
}

#[tokio::test]
async fn empty_conversation_is_noop() {
    let layer = OrphanSnipLayer::new();
    let mut messages: Vec<ChatMessage> = vec![];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
        artifact_store: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
    assert_eq!(report.removed_count, 0);
}
