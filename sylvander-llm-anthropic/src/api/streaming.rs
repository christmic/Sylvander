//! Low-level SSE (Server-Sent Events) byte parser.
//!
//! Converts a stream of byte chunks into a sequence of
//! [`RawStreamEvent`]s. Buffering across chunk boundaries is handled
//! internally — callers can feed arbitrarily-sized chunks and the parser
//! will only yield complete events.
//!
//! ## Wire format
//!
//! ```text
//! event: message_start
//! data: {"type":"message_start","message":{...}}
//!
//! event: content_block_start
//! data: {"type":"content_block_start","index":0,"content_block":{...}}
//!
//! event: ping
//! data: {"type":"ping"}
//! ...
//! ```
//!
//! Each event is `event: <type>\ndata: <json>\n\n`. Lines starting with
//! `:` are comments and ignored. The `data:` field carries the JSON
//! payload.

use crate::api::error::AnthropicError;
use crate::api::types::RawStreamEvent;

/// Stateful SSE parser. Feed byte chunks via [`Self::feed`] and call
/// [`Self::finish`] when the stream ends.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
}

impl SseParser {
    /// Create a new empty parser.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Feed a chunk of bytes. Returns 0 or more complete events; an
    /// incomplete event remains buffered for the next call.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Result<RawStreamEvent, AnthropicError>> {
        // Tolerate non-UTF-8 by treating invalid bytes as replacement
        // chars — Anthropic always emits valid UTF-8 but we don't want a
        // single bad byte to abort the entire stream.
        let chunk_str = String::from_utf8_lossy(chunk);
        self.buffer.push_str(&chunk_str);

        let mut events = Vec::new();
        while let Some(sep_idx) = find_event_separator(&self.buffer) {
            let raw = self.buffer[..sep_idx].to_string();
            // Advance past the separator.
            let sep_len = separator_length(&self.buffer, sep_idx);
            self.buffer.drain(..sep_idx + sep_len);

            match parse_event(&raw) {
                Some(Ok(event)) => events.push(Ok(event)),
                Some(Err(e)) => events.push(Err(e)),
                None => {
                    // Empty event (e.g., a blank line at the start); skip.
                }
            }
        }
        events
    }

    /// Finalize the parser. Returns an error if the buffer contains
    /// non-empty, unparseable content.
    pub fn finish(self) -> Result<(), AnthropicError> {
        if self.buffer.trim().is_empty() {
            Ok(())
        } else {
            Err(AnthropicError::SseParse {
                message: format!("unfinished event in buffer: {:?}", self.buffer),
                position: 0,
            })
        }
    }
}

/// Find the byte index of the next event separator (`\n\n` or `\r\n\r\n`).
fn find_event_separator(s: &str) -> Option<usize> {
    // Search for \n\n or \r\n\r\n.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
            return Some(i);
        }
        if i + 3 < bytes.len()
            && bytes[i] == b'\r'
            && bytes[i + 1] == b'\n'
            && bytes[i + 2] == b'\r'
            && bytes[i + 3] == b'\n'
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Return the length of the separator starting at `idx` (2 for `\n\n`, 4
/// for `\r\n\r\n`).
fn separator_length(s: &str, idx: usize) -> usize {
    let bytes = s.as_bytes();
    if idx + 3 < bytes.len()
        && bytes[idx] == b'\r'
        && bytes[idx + 1] == b'\n'
        && bytes[idx + 2] == b'\r'
        && bytes[idx + 3] == b'\n'
    {
        4
    } else {
        2
    }
}

/// Parse a single SSE event block (without the trailing blank line) into
/// an event. Returns `None` for empty events, `Some(Ok(_))` for valid
/// events, and `Some(Err(_))` for malformed events.
fn parse_event(block: &str) -> Option<Result<RawStreamEvent, AnthropicError>> {
    let mut event_type: Option<&str> = None;
    let mut data_lines: Vec<&str> = Vec::new();

    for line in block.split('\n') {
        // Trim trailing \r for CRLF tolerance.
        let line = line.strip_suffix('\r').unwrap_or(line);

        if line.is_empty() {
            continue;
        }
        if line.starts_with(':') {
            // Comment — ignore.
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("data:") {
            // SSE spec says to strip a single leading space if present.
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            data_lines.push(rest);
        }
        // Other field types (id:, retry:) ignored — Anthropic doesn't use
        // them.
    }

    if data_lines.is_empty() {
        // No data — empty event.
        return None;
    }

    // Anthropic data fields are always a single line of JSON. Multiple
    // lines would mean concatenation per SSE spec, but we don't expect
    // that here.
    let data = data_lines.join("\n");

    let parsed: Result<RawStreamEvent, _> = serde_json::from_str(&data);
    match parsed {
        Ok(event) => {
            // Verify the type field matches the SSE event line (if both
            // are present, they should agree).
            if let Some(t) = event_type {
                let event_type_str = event_type_name(&event);
                if event_type_str != t {
                    return Some(Err(AnthropicError::SseParse {
                        message: format!(
                            "event type mismatch: SSE says '{t}', data says '{event_type_str}'"
                        ),
                        position: 0,
                    }));
                }
            }
            Some(Ok(event))
        }
        Err(e) => Some(Err(AnthropicError::SseParse {
            message: format!("failed to parse event JSON: {e}; data: {data}"),
            position: 0,
        })),
    }
}

/// Return the wire-format event name for a parsed event (e.g.,
/// `"message_start"` for [`RawStreamEvent::MessageStart`]).
fn event_type_name(event: &RawStreamEvent) -> &'static str {
    match event {
        RawStreamEvent::MessageStart { .. } => "message_start",
        RawStreamEvent::ContentBlockStart { .. } => "content_block_start",
        RawStreamEvent::Ping => "ping",
        RawStreamEvent::ContentBlockDelta { .. } => "content_block_delta",
        RawStreamEvent::ContentBlockStop { .. } => "content_block_stop",
        RawStreamEvent::MessageDelta { .. } => "message_delta",
        RawStreamEvent::MessageStop => "message_stop",
    }
}

#[cfg(test)]
mod tests {
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
}
