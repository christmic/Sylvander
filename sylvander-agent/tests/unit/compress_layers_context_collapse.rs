use super::*;
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

fn assistant_with_thinking(thinking: &str) -> MessageParam {
    MessageParam {
        role: MessageRole::Assistant,
        content: UserContent::Blocks(vec![UserContentBlock::Other(json!({
            "type": "thinking",
            "thinking": thinking,
            "signature": "sig_xyz"
        }))]),
    }
}

fn extract_thinking(msg: &MessageParam) -> Option<String> {
    let UserContent::Blocks(blocks) = &msg.content else {
        return None;
    };
    let UserContentBlock::Other(j) = blocks.first()? else {
        return None;
    };
    j.get("thinking")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
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
    };

    layer.apply(&mut ctx).await;
    let UserContent::Blocks(blocks) = &messages[0].content else {
        panic!();
    };
    let UserContentBlock::Other(j) = &blocks[0] else {
        panic!();
    };
    assert_eq!(
        j.get("signature").and_then(JsonValue::as_str),
        Some("sig_xyz")
    );
    assert_eq!(j.get("type").and_then(JsonValue::as_str), Some("thinking"));
}

#[tokio::test]
async fn empty_conversation_is_noop() {
    let layer = ContextCollapseLayer::new();
    let mut messages: Vec<MessageParam> = vec![];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
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
    let mut messages = vec![MessageParam {
        role: MessageRole::User,
        content: UserContent::Blocks(vec![UserContentBlock::Other(json!({
            "type": "tool_use",
            "id": "toolu_x",
            "name": "fake",
            "input": {}
        }))]),
    }];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
    // The tool_use block is intact.
    let UserContent::Blocks(blocks) = &messages[0].content else {
        panic!();
    };
    let UserContentBlock::Other(j) = &blocks[0] else {
        panic!();
    };
    assert_eq!(j.get("type").and_then(JsonValue::as_str), Some("tool_use"));
}
