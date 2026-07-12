//! token9 API wire types. Single source of truth for the client/server contract.
//! Swift types are code-generated from this crate via typeshare — do not hand-write
//! the client-side equivalents.

use serde::{Deserialize, Serialize};
use typeshare::typeshare;

/// One aggregated usage bucket: provider + model + day.
#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatBucketDto {
    pub provider: String,
    pub model: String,
    /// Logical tool label (config-mapped; "OTHER" if unmatched).
    pub tool: String,
    pub date: String,
    // i64 needs an explicit serialized_as for typeshare; I54 -> Swift Int64,
    // JS-safe (token counts never approach 2^54).
    #[typeshare(serialized_as = "I54")]
    pub requests: i64,
    #[typeshare(serialized_as = "I54")]
    pub input_tokens: i64,
    #[typeshare(serialized_as = "I54")]
    pub output_tokens: i64,
    #[typeshare(serialized_as = "I54")]
    pub cache_read_tokens: i64,
    #[typeshare(serialized_as = "I54")]
    pub cache_write_tokens: i64,
    pub cache_ratio: f64,
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    pub buckets: Vec<StatBucketDto>,
}

/// A provider. `api_key` is masked by the server before it reaches the wire.
#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDto {
    pub name: String,
    pub base_url: String,
    pub dialect: String,
    pub api_key: Option<String>,
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersResponse {
    pub providers: Vec<ProviderDto>,
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDto {
    pub model_id: String,
    pub provider: String,
    pub real_model: String,
    pub inject_usage: bool,
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub models: Vec<ModelDto>,
}

/// Latest vendor rate-limit snapshot for a provider, captured from upstream
/// response headers (observe-only). `*_reset` is the raw vendor value
/// (RFC3339 for Anthropic, a duration string for OpenAI).
#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitDto {
    pub provider: String,
    #[typeshare(serialized_as = "I54")]
    pub updated_at: i64,
    // Optional counts kept as i32 so typeshare preserves optionality
    // (serialized_as would drop it); vendor rate-limit counts fit in i32.
    pub requests_limit: Option<i32>,
    pub requests_remaining: Option<i32>,
    pub requests_reset: Option<String>,
    pub tokens_limit: Option<i32>,
    pub tokens_remaining: Option<i32>,
    pub tokens_reset: Option<String>,
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitsResponse {
    pub rate_limits: Vec<RateLimitDto>,
}

/// A configurable tool-identification rule: if request header `header` contains
/// `pattern` (case-insensitive), attribute the request to logical `label`.
#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRuleDto {
    #[typeshare(serialized_as = "I54")]
    pub id: i64,
    pub label: String,
    pub header: String,
    pub pattern: String,
    #[typeshare(serialized_as = "I54")]
    pub priority: i64,
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRulesResponse {
    pub rules: Vec<ToolRuleDto>,
}

/// A distinct real tool identifier seen in traffic + its current logical
/// mapping — used to discover unmapped tools (tool == "OTHER").
#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedToolDto {
    pub tool_raw: String,
    pub tool: String,
    #[typeshare(serialized_as = "I54")]
    pub requests: i64,
}

#[typeshare]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedToolsResponse {
    pub observed: Vec<ObservedToolDto>,
}
