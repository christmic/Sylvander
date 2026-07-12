pub mod config_store;
pub mod sqlite;

use crate::config::Dialect;

/// One metered request. Written off the forwarding path.
#[derive(Debug, Clone)]
pub struct RequestRow {
    pub id: String,
    pub ts: i64,
    pub client_protocol: String,
    pub model_id: String,
    pub provider: String,
    pub real_model: String,
    pub stream: bool,
    pub status: Option<i64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_read_tokens: i64,
    pub latency_ms: Option<i64>,
    pub ttft_ms: Option<i64>,
    pub error: Option<String>,
    /// Logical tool label (config-mapped; "OTHER" if unmatched).
    pub tool: String,
    /// Real tool identifier (raw User-Agent), for discovering unmapped tools.
    pub tool_raw: Option<String>,
    /// Number of upstream attempts made (1 = no fallback).
    pub attempts: i64,
    /// Why the committed target was chosen: primary / load_balance / fallback.
    pub route_reason: Option<String>,
    /// JSON trail of every attempt and its outcome, for analysis.
    pub route_trail: Option<String>,
}

/// A provider row as stored (api_key kept as-is; encryption is a future ConfigStore concern).
#[derive(Debug, Clone)]
pub struct ProviderRow {
    pub name: String,
    pub base_url: String,
    pub dialect: String,
    pub api_key: Option<String>,
}

/// A logical-model row joined with its provider name.
#[derive(Debug, Clone)]
pub struct ModelRow {
    pub model_id: String,
    pub provider: String,
    pub real_model: String,
    pub inject_usage: bool,
}

/// One candidate backend for a logical model, with the provider's usable keys.
#[derive(Debug, Clone)]
pub struct TargetDef {
    pub provider: String,
    pub base_url: String,
    pub dialect: Dialect,
    pub real_model: String,
    pub weight: i64,
    pub priority: i64,
    pub keys: Vec<String>,
}

/// A logical model's full routing set: ordered candidate targets.
#[derive(Debug, Clone)]
pub struct RouteSet {
    pub model_id: String,
    pub inject_usage: bool,
    pub targets: Vec<TargetDef>,
}

/// A provider_keys row (api_key masked by callers when displayed).
#[derive(Debug, Clone)]
pub struct ProviderKeyRow {
    pub id: i64,
    pub provider: String,
    pub api_key: Option<String>,
    pub label: Option<String>,
    pub enabled: bool,
}

/// A routes row.
#[derive(Debug, Clone)]
pub struct RouteRow {
    pub id: i64,
    pub model_id: String,
    pub provider: String,
    pub real_model: String,
    pub weight: i64,
    pub priority: i64,
    pub enabled: bool,
}

/// One aggregated stats bucket (provider + model + day).
#[derive(Debug, Clone)]
pub struct StatBucket {
    pub provider: String,
    pub real_model: String,
    pub tool: String,
    pub date: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

/// A configurable tool-identification rule (persisted).
#[derive(Debug, Clone)]
pub struct ToolRuleRow {
    pub id: i64,
    pub label: String,
    pub header: String,
    pub pattern: String,
    pub priority: i64,
}

/// A distinct real tool identifier observed in traffic, with its current
/// logical mapping — used to discover unmapped tools (logical == "OTHER").
#[derive(Debug, Clone)]
pub struct ObservedTool {
    pub tool_raw: String,
    pub tool: String,
    pub requests: i64,
}

/// Latest vendor rate-limit snapshot for a provider (captured from response headers).
#[derive(Debug, Clone)]
pub struct RateLimitRow {
    pub provider: String,
    pub updated_at: i64,
    pub requests_limit: Option<i64>,
    pub requests_remaining: Option<i64>,
    pub requests_reset: Option<String>,
    pub tokens_limit: Option<i64>,
    pub tokens_remaining: Option<i64>,
    pub tokens_reset: Option<String>,
}
