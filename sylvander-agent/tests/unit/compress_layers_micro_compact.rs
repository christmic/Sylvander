use super::*;
use sylvander_llm_core::{
    ChatMessage, ContentBlock, ModelCapabilities, ModelInfo, ModelRef, TokenUsage,
    ToolResultContent,
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

fn user_msg_text(text: &str) -> ChatMessage {
    ChatMessage::user(text)
}

fn user_msg_with_tool_result(tool_use_id: &str, body: &str) -> ChatMessage {
    ChatMessage::user_blocks(vec![ContentBlock::tool_result_text(
        tool_use_id,
        body,
        false,
    )])
}

fn first_tool_result_body(msg: &ChatMessage) -> Option<String> {
    let ContentBlock::ToolResult { content, .. } = msg.content.first()? else {
        return None;
    };
    match content.first()? {
        ToolResultContent::Text { text } => Some(text.clone()),
        _ => None,
    }
}

#[tokio::test]
async fn keeps_last_n_user_messages_intact() {
    let layer = MicroCompactLayer::new().with_keep_last_n(2);
    let mut messages = vec![
        user_msg_with_tool_result("old_1", "x".repeat(200).as_str()),
        user_msg_with_tool_result("old_2", "y".repeat(200).as_str()),
        user_msg_with_tool_result("recent_1", "z".repeat(200).as_str()),
        user_msg_with_tool_result("recent_2", "w".repeat(200).as_str()),
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
    // The two old ones got placeholders; the two recent ones
    // are intact.
    let body0 = first_tool_result_body(&messages[0]).unwrap();
    assert!(body0.contains("truncated"), "old_1 should be condensed");
    let body2 = first_tool_result_body(&messages[2]).unwrap();
    assert!(
        !body2.contains("truncated"),
        "recent_1 should be intact, got: {body2}"
    );
}

#[tokio::test]
async fn does_not_affect_user_text_messages() {
    let layer = MicroCompactLayer::new().with_keep_last_n(0);
    let mut messages = vec![user_msg_text("user plain text")];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
        artifact_store: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
    // User text is unchanged.
    let [ContentBlock::Text { text: s }] = messages[0].content.as_slice() else {
        panic!("expected string");
    };
    assert_eq!(s, "user plain text");
}

#[tokio::test]
async fn zero_keep_condenses_all_tool_results() {
    let layer = MicroCompactLayer::new().with_keep_last_n(0);
    let mut messages = vec![
        user_msg_with_tool_result("a", "x".repeat(100).as_str()),
        user_msg_with_tool_result("b", "y".repeat(100).as_str()),
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
    assert!(report.freed_tokens > 0);
}

#[tokio::test]
async fn empty_conversation_is_noop() {
    let layer = MicroCompactLayer::new();
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
}

#[tokio::test]
async fn short_tool_results_not_rewritten() {
    // If a tool_result body is already shorter than the
    // placeholder, don't bother rewriting.
    let layer = MicroCompactLayer::new().with_keep_last_n(0);
    let mut messages = vec![user_msg_with_tool_result("a", "short")];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
        artifact_store: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
}
