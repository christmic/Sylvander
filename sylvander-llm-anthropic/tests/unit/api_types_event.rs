use super::*;
use serde_json::json;

#[test]
fn message_start_round_trip() {
    let json = json!({
        "type": "message_start",
        "message": {
            "id": "msg_abc",
            "type": "message",
            "role": "assistant",
            "content": [],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": null,
            "usage": {"input_tokens": 5, "output_tokens": 1}
        }
    });
    let event: RawStreamEvent = serde_json::from_value(json).unwrap();
    match event {
        RawStreamEvent::MessageStart { message } => {
            assert_eq!(message.id, "msg_abc");
        }
        other => panic!("expected MessageStart, got {other:?}"),
    }
}

#[test]
fn content_block_delta_text() {
    let json = json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "text_delta", "text": "Hello"}
    });
    let event: RawStreamEvent = serde_json::from_value(json).unwrap();
    match event {
        RawStreamEvent::ContentBlockDelta { index, delta } => {
            assert_eq!(index, 0);
            match delta {
                ContentDelta::TextDelta { text } => assert_eq!(text, "Hello"),
                other => panic!("expected TextDelta, got {other:?}"),
            }
        }
        other => panic!("expected ContentBlockDelta, got {other:?}"),
    }
}

#[test]
fn content_block_delta_input_json() {
    let json = json!({
        "type": "content_block_delta",
        "index": 1,
        "delta": {"type": "input_json_delta", "partial_json": "{\"loc"}
    });
    let event: RawStreamEvent = serde_json::from_value(json).unwrap();
    match event {
        RawStreamEvent::ContentBlockDelta { index, delta } => {
            assert_eq!(index, 1);
            match delta {
                ContentDelta::InputJsonDelta { partial_json } => {
                    assert_eq!(partial_json, "{\"loc");
                }
                other => panic!("expected InputJsonDelta, got {other:?}"),
            }
        }
        other => panic!("expected ContentBlockDelta, got {other:?}"),
    }
}

#[test]
fn ping_round_trip() {
    let json = json!({"type": "ping"});
    let event: RawStreamEvent = serde_json::from_value(json).unwrap();
    assert!(matches!(event, RawStreamEvent::Ping));
}

#[test]
fn message_delta_round_trip() {
    let json = json!({
        "type": "message_delta",
        "delta": {"stop_reason": "end_turn"},
        "usage": {"output_tokens": 50}
    });
    let event: RawStreamEvent = serde_json::from_value(json).unwrap();
    match event {
        RawStreamEvent::MessageDelta { delta, usage } => {
            assert_eq!(delta.stop_reason, Some(StopReason::EndTurn));
            assert_eq!(usage.output_tokens, 50);
        }
        other => panic!("expected MessageDelta, got {other:?}"),
    }
}

#[test]
fn message_stop_round_trip() {
    let json = json!({"type": "message_stop"});
    let event: RawStreamEvent = serde_json::from_value(json).unwrap();
    assert!(matches!(event, RawStreamEvent::MessageStop));
}
