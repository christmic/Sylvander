use super::{SseEvent, SseParser};

#[test]
fn frames_crlf_events_across_arbitrary_chunks() {
    let mut parser = SseParser::default();
    assert!(parser.feed(b"event: response.output_").is_empty());
    let events = parser.feed(b"text.delta\r\ndata: {\"delta\":\"hi\"}\r\n\r\n");
    assert_eq!(
        events.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        vec![SseEvent {
            event: Some("response.output_text.delta".into()),
            data: "{\"delta\":\"hi\"}".into(),
        }]
    );
    parser.finish().expect("complete parser");
}

#[test]
fn joins_multiple_data_lines_and_ignores_comments() {
    let mut parser = SseParser::default();
    let events = parser.feed(b": heartbeat\ndata: first\ndata: second\n\n");
    assert_eq!(
        events.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        vec![SseEvent {
            event: None,
            data: "first\nsecond".into(),
        }]
    );
}

#[test]
fn unfinished_event_is_an_error() {
    let mut parser = SseParser::default();
    assert!(parser.feed(b"data: partial").is_empty());
    assert!(parser.finish().is_err());
}
