//! Provider-qualified model catalog and user-visible reasoning DTOs.
//!
//! Runtime maps these stable public values to provider-neutral model metadata
//! and then to the selected official provider adapter. Provider wire details
//! never enter this service contract.

use serde::{Deserialize, Serialize};

/// User-facing reasoning intensity. The runtime maps these stable semantic
/// levels to provider-specific token budgets.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    #[must_use]
    pub fn budget_tokens(self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::Low => Some(2_048),
            Self::Medium => Some(8_192),
            Self::High => Some(20_000),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelDescriptor {
    pub id: String,
    pub provider: String,
    /// Compact capability bitset used by terminal clients.
    pub capabilities: u8,
    /// Provider-neutral, canonical capabilities for current clients.
    pub capability_names: Vec<ModelCapability>,
    pub reasoning_efforts: Vec<ReasoningEffort>,
    pub lifecycle: ModelLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
}

/// Canonical model capabilities exposed by the public protocol.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    ExtendedThinking,
    PromptCaching,
    StructuredOutput,
    ToolUse,
    Vision,
    DocumentInput,
    AudioInput,
}

/// Stable identity for one model exposed by one provider.
///
/// Model ids are not globally unique. Persisted selections and new wire
/// requests therefore use both fields as one indivisible identity.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelSelectionResolutionError {
    #[error("model selection `{provider_id}/{model_id}` is unavailable")]
    Unavailable {
        provider_id: String,
        model_id: String,
    },
}

/// Operator-supplied API prices in micro-US-dollars per million tokens.
/// `1_000_000` therefore means `$1.00 / 1M tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelPricing {
    pub input_usd_micros_per_million: u64,
    pub output_usd_micros_per_million: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_usd_micros_per_million: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_usd_micros_per_million: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ModelLifecycle {
    #[default]
    Active,
    Deprecated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replacement: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RuntimeModelInfo {
    pub current: ModelSelection,
    pub reasoning_effort: ReasoningEffort,
    pub models: Vec<ModelDescriptor>,
}

#[cfg(test)]
#[path = "../tests/unit/model.rs"]
mod tests;
