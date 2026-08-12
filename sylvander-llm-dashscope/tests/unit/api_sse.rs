use super::{SseEvent, SseParser};

#[test]
fn preserves_dashscope_error_event_and_status() {
    let mut parser = SseParser::default();
    let events =
        parser.feed(b"event:error\r\nstatus:429\r\ndata:{\"code\":\"Throttling\"}\r\n\r\n");
    assert_eq!(
        events.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        vec![SseEvent {
            kind: Some("error".into()),
            status: Some(429),
            data: "{\"code\":\"Throttling\"}".into(),
        }]
    );
}

#[test]
fn rejects_non_numeric_status() {
    let mut parser = SseParser::default();
    let events = parser.feed(b"event:error\nstatus:nope\ndata:{}\n\n");
    assert!(events[0].is_err());
}

#[test]
fn buffers_split_utf8_without_loss() {
    let mut parser = SseParser::default();
    let bytes = "data:{\"text\":\"杭州\"}\n\n".as_bytes();
    assert!(parser.feed(&bytes[..16]).is_empty());
    let events = parser.feed(&bytes[16..]);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].as_ref().expect("event").data,
        "{\"text\":\"杭州\"}"
    );
}
