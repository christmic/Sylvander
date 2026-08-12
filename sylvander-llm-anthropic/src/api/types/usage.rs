//! Token usage accounting.

use serde::{Deserialize, Serialize};

/// Token usage returned by the Messages API. Reported on every successful
/// response (sync) and accumulated in `message_delta` events (streaming).
///
/// The total billable input tokens for a request is the sum of
/// `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Breakdown of cache creation input tokens by cache lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<CacheCreation>,
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
    /// Geographic region where inference was performed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_geo: Option<String>,
    /// Breakdown of generated output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<OutputTokensDetails>,
    /// Number of Anthropic-hosted tool requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_tool_use: Option<ServerToolUsage>,
    /// Service tier used for the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
}

/// Cache creation token counts split by the configured ephemeral lifetime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheCreation {
    /// Input tokens written to a one-hour cache entry.
    pub ephemeral_1h_input_tokens: u32,
    /// Input tokens written to a five-minute cache entry.
    pub ephemeral_5m_input_tokens: u32,
}

/// Detailed output token accounting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputTokensDetails {
    /// Output tokens spent on internal thinking.
    pub thinking_tokens: u32,
}

/// Anthropic-hosted tool request counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToolUsage {
    /// Number of web fetch requests.
    pub web_fetch_requests: u32,
    /// Number of web search requests.
    pub web_search_requests: u32,
}

/// Service tier selected by Anthropic for the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTier {
    /// Standard service tier.
    Standard,
    /// Priority service tier.
    Priority,
    /// Batch processing tier.
    Batch,
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
