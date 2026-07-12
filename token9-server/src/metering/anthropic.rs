use super::{Usage, as_i64};

/// Extract Anthropic usage from a single JSON object (SSE event or full response).
///
/// - `message_start` carries `message.usage` with input + cache counts.
/// - `message_delta` carries `usage.output_tokens` (cumulative final output).
/// - Non-streaming responses carry a top-level `usage` with all fields.
pub fn apply(json: &serde_json::Value, usage: &mut Usage) {
    // message_start: usage nested under `message`.
    if let Some(u) = json.get("message").and_then(|m| m.get("usage")) {
        read_into(u, usage);
    }
    // message_delta / non-streaming: top-level `usage`.
    if let Some(u) = json.get("usage") {
        read_into(u, usage);
    }
}

fn read_into(u: &serde_json::Value, usage: &mut Usage) {
    if let Some(v) = as_i64(u.get("input_tokens")) {
        usage.input = v.max(usage.input);
    }
    if let Some(v) = as_i64(u.get("output_tokens")) {
        usage.output = v.max(usage.output);
    }
    if let Some(v) = as_i64(u.get("cache_creation_input_tokens")) {
        usage.cache_write = v.max(usage.cache_write);
    }
    if let Some(v) = as_i64(u.get("cache_read_input_tokens")) {
        usage.cache_read = v.max(usage.cache_read);
    }
}
