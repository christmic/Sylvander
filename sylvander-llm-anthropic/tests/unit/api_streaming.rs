use super::*;
use crate::api::types::StopReason;
use serde_json::json;

fn feed_all(parser: &mut SseParser, s: &str) -> Vec<Result<RawStreamEvent, AnthropicError>> {
    parser.feed(s.as_bytes())
}

#[test]
fn parses_single_complete_event() {
    let mut p = SseParser::new();
    let event_body = json!({"type": "ping"}).to_string();
    let chunk = format!("event: ping\ndata: {event_body}\n\n");
    let events = feed_all(&mut p, &chunk);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Ok(RawStreamEvent::Ping)));
    p.finish().unwrap();
}

#[test]
fn parses_event_split_across_chunks() {
    let mut p = SseParser::new();
    let events = feed_all(&mut p, "event: pin");
    assert!(events.is_empty());
    let events = feed_all(&mut p, "g\ndata: {\"type\":\"ping\"}\n\n");
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Ok(RawStreamEvent::Ping)));
    p.finish().unwrap();
}

#[test]
fn parses_multiple_events_in_one_chunk() {
    let mut p = SseParser::new();
    let chunk = "\
event: ping
data: {\"type\":\"ping\"}

event: message_stop
data: {\"type\":\"message_stop\"}

";
    let events = feed_all(&mut p, chunk);
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], Ok(RawStreamEvent::Ping)));
    assert!(matches!(events[1], Ok(RawStreamEvent::MessageStop)));
    p.finish().unwrap();
}

#[test]
fn handles_crlf_line_endings() {
    let mut p = SseParser::new();
    let chunk = "event: ping\r\ndata: {\"type\":\"ping\"}\r\n\r\n";
    let events = feed_all(&mut p, chunk);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Ok(RawStreamEvent::Ping)));
    p.finish().unwrap();
}

#[test]
fn ignores_comment_lines() {
    let mut p = SseParser::new();
    let chunk = ": this is a comment\nevent: ping\ndata: {\"type\":\"ping\"}\n\n";
    let events = feed_all(&mut p, chunk);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Ok(RawStreamEvent::Ping)));
    p.finish().unwrap();
}

#[test]
fn skips_empty_events() {
    let mut p = SseParser::new();
    let chunk = "\n\nevent: ping\ndata: {\"type\":\"ping\"}\n\n";
    let events = feed_all(&mut p, chunk);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Ok(RawStreamEvent::Ping)));
    p.finish().unwrap();
}

#[test]
fn parses_content_block_delta_text() {
    let mut p = SseParser::new();
    let chunk = "\
event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}

";
    let events = feed_all(&mut p, chunk);
    assert_eq!(events.len(), 1);
    match &events[0] {
        Ok(RawStreamEvent::ContentBlockDelta { index, delta }) => {
            assert_eq!(*index, 0);
            match delta {
                crate::api::types::ContentDelta::TextDelta { text } => {
                    assert_eq!(text, "Hi");
                }
                other => panic!("expected TextDelta, got {other:?}"),
            }
        }
        other => panic!("expected ContentBlockDelta, got {other:?}"),
    }
    p.finish().unwrap();
}

#[test]
fn detects_event_type_mismatch() {
    let mut p = SseParser::new();
    let chunk = "event: ping\ndata: {\"type\":\"message_stop\"}\n\n";
    let events = feed_all(&mut p, chunk);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Err(AnthropicError::SseParse { .. })));
}

#[test]
fn malformed_json_yields_parse_error() {
    let mut p = SseParser::new();
    let chunk = "event: ping\ndata: {not valid json}\n\n";
    let events = feed_all(&mut p, chunk);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Err(AnthropicError::SseParse { .. })));
}

#[test]
fn finish_with_unfinished_event_errors() {
    let mut p = SseParser::new();
    feed_all(&mut p, "event: ping\ndata: {\"ty");
    let result = p.finish();
    assert!(matches!(result, Err(AnthropicError::SseParse { .. })));
}

#[test]
fn parses_full_message_stream() {
    let mut p = SseParser::new();
    let chunk = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_x\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-5-20260601\",\"stop_reason\":null,\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":0}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}

event: message_stop
data: {\"type\":\"message_stop\"}

";
    let events = feed_all(&mut p, chunk);
    assert_eq!(events.len(), 6);
    assert!(matches!(events[0], Ok(RawStreamEvent::MessageStart { .. })));
    assert!(matches!(
        events[1],
        Ok(RawStreamEvent::ContentBlockStart { .. })
    ));
    assert!(matches!(
        events[2],
        Ok(RawStreamEvent::ContentBlockDelta { .. })
    ));
    assert!(matches!(
        events[3],
        Ok(RawStreamEvent::ContentBlockStop { .. })
    ));
    match &events[4] {
        Ok(RawStreamEvent::MessageDelta { delta, usage }) => {
            assert_eq!(delta.stop_reason, Some(StopReason::EndTurn));
            assert_eq!(usage.output_tokens, 5);
        }
        other => panic!("expected MessageDelta, got {other:?}"),
    }
    assert!(matches!(events[5], Ok(RawStreamEvent::MessageStop)));
    p.finish().unwrap();
}
