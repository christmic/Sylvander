//! OpenAI Chat Completions and Responses usage extraction.

use super::{Usage, as_i64};

/// Extract OpenAI usage from a single JSON object.
///
/// - Chat Completions / non-streaming: top-level `usage`
///   (`prompt_tokens`, `completion_tokens`, `prompt_tokens_details.cached_tokens`).
/// - Responses API: usage under `response.usage`
///   (`input_tokens`, `output_tokens`, `input_tokens_details.cached_tokens`).
pub fn apply(json: &serde_json::Value, usage: &mut Usage) {
    if let Some(u) = json.get("usage") {
        read_into(u, usage);
    }
    if let Some(u) = json.get("response").and_then(|r| r.get("usage")) {
        read_into(u, usage);
    }
}

fn read_into(u: &serde_json::Value, usage: &mut Usage) {
    // input: prompt_tokens (chat) or input_tokens (responses)
    if let Some(v) = as_i64(u.get("prompt_tokens")).or_else(|| as_i64(u.get("input_tokens"))) {
        usage.input = v.max(usage.input);
    }
    // output: completion_tokens (chat) or output_tokens (responses)
    if let Some(v) = as_i64(u.get("completion_tokens")).or_else(|| as_i64(u.get("output_tokens"))) {
        usage.output = v.max(usage.output);
    }
    // cache read: prompt_tokens_details or input_tokens_details -> cached_tokens
    let cached = u
        .get("prompt_tokens_details")
        .or_else(|| u.get("input_tokens_details"))
        .and_then(|d| as_i64(d.get("cached_tokens")));
    if let Some(v) = cached {
        usage.cache_read = v.max(usage.cache_read);
    }
}
