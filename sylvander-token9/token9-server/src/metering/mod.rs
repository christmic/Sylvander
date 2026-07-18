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
#[path = "../../tests/unit/metering.rs"]
mod tests;
