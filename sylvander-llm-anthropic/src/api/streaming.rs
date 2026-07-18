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
#[path = "../../tests/unit/api_streaming.rs"]
mod tests;
