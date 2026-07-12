pub mod anthropic;
pub mod openai;

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::warn;

use crate::config::Dialect;
use crate::store::{RequestRow, sqlite::SqliteStore};

/// Accumulated token usage extracted from an upstream response.
#[derive(Debug, Default, Clone)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
    pub cache_write: i64,
    pub cache_read: i64,
}

/// Immutable request facts handed to the off-path metering task.
#[derive(Debug, Clone)]
pub struct MeterMeta {
    pub id: String,
    pub ts: i64,
    pub start: Instant,
    pub dialect: Dialect,
    pub model_id: String,
    pub provider: String,
    pub real_model: String,
    pub stream: bool,
    pub status: i64,
    pub tool: String,
    pub tool_raw: Option<String>,
    pub attempts: i64,
    pub route_reason: Option<String>,
    pub route_trail: Option<String>,
}

/// Consume cloned response bytes off the forwarding path, parse usage, persist.
/// Never touches or blocks the client stream (§1.5 #7).
pub async fn run(mut rx: UnboundedReceiver<Bytes>, meta: MeterMeta, store: Arc<SqliteStore>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut first_chunk: Option<Instant> = None;

    while let Some(chunk) = rx.recv().await {
        if first_chunk.is_none() {
            first_chunk = Some(Instant::now());
        }
        buf.extend_from_slice(&chunk);
    }

    let ttft_ms = first_chunk.map(|t| t.saturating_duration_since(meta.start).as_millis() as i64);
    let latency_ms = meta.start.elapsed().as_millis() as i64;
    let usage = parse(meta.dialect, &buf);

    let row = RequestRow {
        id: meta.id,
        ts: meta.ts,
        client_protocol: meta.dialect.as_str().to_string(),
        model_id: meta.model_id,
        provider: meta.provider,
        real_model: meta.real_model,
        stream: meta.stream,
        status: Some(meta.status),
        input_tokens: usage.input,
        output_tokens: usage.output,
        cache_write_tokens: usage.cache_write,
        cache_read_tokens: usage.cache_read,
        latency_ms: Some(latency_ms),
        ttft_ms,
        error: None,
        tool: meta.tool,
        tool_raw: meta.tool_raw,
        attempts: meta.attempts,
        route_reason: meta.route_reason,
        route_trail: meta.route_trail,
    };

    if let Err(e) = store.record(row).await {
        warn!(error = %e, "failed to record usage");
    }
}

/// Parse usage from a full response buffer. Handles both SSE and single-JSON bodies.
/// Note: assumes an uncompressed body; gzip/brotli responses are not decoded here.
pub fn parse(dialect: Dialect, buf: &[u8]) -> Usage {
    let text = String::from_utf8_lossy(buf);
    let mut usage = Usage::default();

    let looks_sse = text.contains("data:");
    if looks_sse {
        for line in text.lines() {
            let line = line.trim_start();
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) {
                apply(dialect, &json, &mut usage);
            }
        }
    } else if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
        apply(dialect, &json, &mut usage);
    }

    usage
}

fn apply(dialect: Dialect, json: &serde_json::Value, usage: &mut Usage) {
    match dialect {
        Dialect::Anthropic => anthropic::apply(json, usage),
        Dialect::OpenaiChat | Dialect::OpenaiResponses => openai::apply(json, usage),
    }
}

/// Read a non-negative integer from a JSON value, if present.
pub(crate) fn as_i64(v: Option<&serde_json::Value>) -> Option<i64> {
    v.and_then(|x| x.as_i64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Dialect;

    #[test]
    fn anthropic_streaming() {
        let sse = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude\",\"usage\":{\"input_tokens\":25,\"cache_creation_input_tokens\":10,\"cache_read_input_tokens\":5,\"output_tokens\":1}}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}

event: message_stop
data: {\"type\":\"message_stop\"}
";
        let u = parse(Dialect::Anthropic, sse.as_bytes());
        assert_eq!(u.input, 25);
        assert_eq!(u.output, 42);
        assert_eq!(u.cache_write, 10);
        assert_eq!(u.cache_read, 5);
    }

    #[test]
    fn anthropic_non_streaming() {
        let json = r#"{"type":"message","model":"claude","content":[{"type":"text","text":"Hi"}],"usage":{"input_tokens":30,"output_tokens":15,"cache_creation_input_tokens":0,"cache_read_input_tokens":20}}"#;
        let u = parse(Dialect::Anthropic, json.as_bytes());
        assert_eq!(u.input, 30);
        assert_eq!(u.output, 15);
        assert_eq!(u.cache_write, 0);
        assert_eq!(u.cache_read, 20);
    }

    #[test]
    fn openai_chat_streaming_with_usage() {
        let sse = "\
data: {\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}}]}

data: {\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}

data: {\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":8,\"prompt_tokens_details\":{\"cached_tokens\":4}}}

data: [DONE]
";
        let u = parse(Dialect::OpenaiChat, sse.as_bytes());
        assert_eq!(u.input, 12);
        assert_eq!(u.output, 8);
        assert_eq!(u.cache_read, 4);
        assert_eq!(u.cache_write, 0);
    }

    #[test]
    fn openai_responses_streaming() {
        let sse = "\
data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}

data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":50,\"output_tokens\":20,\"input_tokens_details\":{\"cached_tokens\":10}}}}
";
        let u = parse(Dialect::OpenaiResponses, sse.as_bytes());
        assert_eq!(u.input, 50);
        assert_eq!(u.output, 20);
        assert_eq!(u.cache_read, 10);
    }

    #[test]
    fn openai_chat_non_streaming() {
        let json = r#"{"object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"Hi"}}],"usage":{"prompt_tokens":7,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":0}}}"#;
        let u = parse(Dialect::OpenaiChat, json.as_bytes());
        assert_eq!(u.input, 7);
        assert_eq!(u.output, 3);
    }

    #[test]
    fn empty_or_unparseable_yields_zero() {
        let u = parse(Dialect::Anthropic, b"");
        assert_eq!(u.input, 0);
        assert_eq!(u.output, 0);
        let u2 = parse(Dialect::OpenaiChat, b"not json at all");
        assert_eq!(u2.input, 0);
    }
}
