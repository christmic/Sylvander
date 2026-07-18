use super::*;
use crate::api::types::block::TextBlock;
use serde_json::json;

#[test]
fn user_message_string_round_trip() {
    let m = MessageParam::user("Hello");
    let json = serde_json::to_string(&m).unwrap();
    assert_eq!(json, r#"{"role":"user","content":"Hello"}"#);
    let back: MessageParam = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

#[test]
fn user_message_blocks_round_trip() {
    let m = MessageParam::user_blocks(vec![crate::api::types::UserContentBlock::text(
        "multi\nline",
    )]);
    let json = serde_json::to_string(&m).unwrap();
    let back: MessageParam = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

#[test]
fn message_response_round_trip() {
    let m = Message {
        id: "msg_abc".to_string(),
        kind: MessageKind::Message,
        role: MessageRole::Assistant,
        content: vec![ContentBlock::Text(TextBlock::new("Hello!"))],
        model: "claude-sonnet-5-20260601".to_string(),
        stop_reason: Some(StopReason::EndTurn),
        stop_sequence: None,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
    };
    let json = serde_json::to_string(&m).unwrap();
    let back: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

#[test]
fn message_text_concat() {
    let m = Message {
        id: "msg_x".to_string(),
        kind: MessageKind::Message,
        role: MessageRole::Assistant,
        content: vec![
            ContentBlock::Text(TextBlock::new("Hello, ")),
            ContentBlock::Text(TextBlock::new("world!")),
        ],
        model: "claude-sonnet-5-20260601".to_string(),
        stop_reason: Some(StopReason::EndTurn),
        stop_sequence: None,
        usage: Usage {
            input_tokens: 0,
            output_tokens: 5,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
    };
    assert_eq!(m.text(), "Hello, world!");
}

#[test]
fn tokens_count_round_trip() {
    let tc = MessageTokensCount {
        input_tokens: 42,
        extra: json!({}),
    };
    let json = serde_json::to_string(&tc).unwrap();
    assert_eq!(json, r#"{"input_tokens":42}"#);
    let back: MessageTokensCount = serde_json::from_str(&json).unwrap();
    assert_eq!(back.input_tokens, 42);
}
