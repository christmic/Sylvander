//! Stateful UTF-8 Server-Sent Events framing shared by `OpenAI` APIs.

use crate::api::OpenAiError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub(crate) struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    pub(super) fn feed(&mut self, chunk: &[u8]) -> Vec<Result<SseEvent, OpenAiError>> {
        self.buffer.extend_from_slice(chunk);
        let mut output = Vec::new();
        while let Some((end, separator)) = boundary(&self.buffer) {
            let raw = self.buffer.drain(..end).collect::<Vec<_>>();
            self.buffer.drain(..separator);
            if let Some(event) = parse(&raw) {
                output.push(event);
            }
        }
        output
    }

    pub(super) fn finish(self) -> Result<(), OpenAiError> {
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err(OpenAiError::Sse(
                "stream ended with an unfinished event".into(),
            ))
        }
    }
}

fn boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|value| value == b"\n\n");
    let crlf = buffer.windows(4).position(|value| value == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(index), None) => Some((index, 2)),
        (None, Some(index)) => Some((index, 4)),
        (None, None) => None,
    }
}

fn parse(raw: &[u8]) -> Option<Result<SseEvent, OpenAiError>> {
    let Ok(text) = std::str::from_utf8(raw) else {
        return Some(Err(OpenAiError::Sse("event was not UTF-8".into())));
    };
    let mut event = None;
    let mut data = Vec::new();
    for line in text.lines() {
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim_start().to_owned());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    (!data.is_empty()).then(|| {
        Ok(SseEvent {
            event,
            data: data.join("\n"),
        })
    })
}

#[cfg(test)]
#[path = "../../tests/unit/api_sse.rs"]
mod tests;
