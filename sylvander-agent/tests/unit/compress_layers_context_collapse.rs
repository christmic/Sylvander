use super::*;
use sylvander_llm_core::{
    ChatMessage, ChatRole, ContentBlock, ModelCapabilities, ModelInfo, ModelRef,
    OpaqueProviderState, TokenUsage,
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

fn assistant_with_thinking(thinking: &str) -> ChatMessage {
    ChatMessage::assistant(vec![ContentBlock::Reasoning {
        text: thinking.into(),
        opaque_state: Some(OpaqueProviderState {
            provider: "test".into(),
            data: json!({"signature": "sig_xyz"}),
        }),
    }])
}

fn extract_thinking(msg: &ChatMessage) -> Option<String> {
    let ContentBlock::Reasoning { text, .. } = msg.content.first()? else {
        return None;
    };
    Some(text.clone())
}

#[tokio::test]
async fn trims_old_thinking_blocks() {
    let layer = ContextCollapseLayer::new()
        .with_keep_last_n(1)
        .with_max_thinking_chars(100);
    let long_thinking = "x".repeat(500);
    let mut messages = vec![
        assistant_with_thinking(&long_thinking),
        assistant_with_thinking(&long_thinking),
        assistant_with_thinking(&long_thinking),
    ];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
        artifact_store: None,
    };

    let report = layer.apply(&mut ctx).await;
    // The 2 oldest get trimmed (keep_last_n=1).
    assert_eq!(report.condensed_count, 2);
    assert!(report.freed_tokens > 0);

    let s0 = extract_thinking(&messages[0]).unwrap();
    assert!(s0.contains("earlier reasoning omitted"));
    assert!(s0.contains("500"));

    let s2 = extract_thinking(&messages[2]).unwrap();
    // The most recent stays intact.
    assert_eq!(s2, long_thinking);
}

#[tokio::test]
async fn preserves_short_thinking() {
    let layer = ContextCollapseLayer::new()
        .with_keep_last_n(0)
        .with_max_thinking_chars(100);
    let short = "brief reasoning";
    let mut messages = vec![assistant_with_thinking(short)];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
        artifact_store: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
    // Short thinking stays as-is.
    assert_eq!(extract_thinking(&messages[0]).unwrap(), short);
}

#[tokio::test]
async fn preserves_signature_field() {
    // The signature field must survive — the API uses it to
    // verify the thinking block.
    let layer = ContextCollapseLayer::new()
        .with_keep_last_n(0)
        .with_max_thinking_chars(50);
    let long = "y".repeat(500);
    let mut messages = vec![assistant_with_thinking(&long)];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
        artifact_store: None,
    };

    layer.apply(&mut ctx).await;
    let ContentBlock::Reasoning {
        opaque_state: Some(state),
        ..
    } = &messages[0].content[0]
    else {
        panic!();
    };
    assert_eq!(state.data["signature"], "sig_xyz");
}

#[tokio::test]
async fn empty_conversation_is_noop() {
    let layer = ContextCollapseLayer::new();
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
    assert!(report.failure.is_none());
}

#[tokio::test]
async fn user_messages_with_other_content_untouched() {
    // User messages shouldn't have thinking blocks, but verify
    // L3 doesn't accidentally damage user tool_result blocks
    // that happen to be wrapped in Other(json).
    let layer = ContextCollapseLayer::new()
        .with_keep_last_n(0)
        .with_max_thinking_chars(50);
    let mut messages = vec![ChatMessage {
        role: ChatRole::User,
        content: vec![ContentBlock::ToolCall {
            id: "toolu_x".into(),
            name: "fake".into(),
            arguments: json!({}),
        }],
    }];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
        artifact_store: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
    // The tool_use block is intact.
    assert!(matches!(
        messages[0].content[0],
        ContentBlock::ToolCall { .. }
    ));
}
