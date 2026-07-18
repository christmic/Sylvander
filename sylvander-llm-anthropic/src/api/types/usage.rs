//! Token usage accounting.

use serde::{Deserialize, Serialize};

/// Token usage returned by the Messages API. Reported on every successful
/// response (sync) and accumulated in `message_delta` events (streaming).
///
/// The total billable input tokens for a request is the sum of
/// `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens billed at full price.
    pub input_tokens: u32,
    /// Output tokens generated.
    pub output_tokens: u32,
    /// Input tokens used to create new cache entries (billed at cache-write
    /// rate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// Input tokens read from a cache hit (billed at cache-read rate,
    /// typically 10% of full price).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

impl Usage {
    /// Total input tokens billed for this request (input + cache creation +
    /// cache read).
    #[must_use]
    pub fn total_input_tokens(&self) -> u32 {
        self.input_tokens
            .saturating_add(self.cache_creation_input_tokens.unwrap_or(0))
            .saturating_add(self.cache_read_input_tokens.unwrap_or(0))
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/api_types_usage.rs"]
mod tests;
