//! Structured output configuration.
//!
//! See [Anthropic structured outputs docs](https://platform.claude.com/docs/en/build-with-claude/structured-outputs).

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Output configuration. Attach to a request to constrain the model's
/// output shape (e.g., force JSON Schema compliance).
///
/// Requires the `structured-outputs-2025-06-01` beta header, which the
/// client adds automatically when this field is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OutputConfig {
    /// Reasoning effort level. Higher effort costs more tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// JSON schema constraint for the output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<JsonOutputFormat>,
}

/// Reasoning effort level for the model's internal reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    /// Prefer the smallest reasoning budget supported by the selected model.
    Low,
    /// Use the provider's balanced reasoning budget.
    Medium,
    /// Prefer a larger reasoning budget for difficult tasks.
    High,
    /// Maximum effort. Newest models only.
    Xhigh,
    /// Absolute maximum effort.
    Max,
}

/// Constrain the response to a JSON schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonOutputFormat {
    /// The JSON schema the response must conform to.
    pub schema: JsonValue,
    /// Always `"json_schema"` for now.
    #[serde(rename = "type")]
    pub kind: JsonOutputFormatKind,
}

impl JsonOutputFormat {
    /// Create a JSON schema output format from a JSON Schema value.
    #[must_use]
    pub fn new(schema: JsonValue) -> Self {
        Self {
            schema,
            kind: JsonOutputFormatKind::JsonSchema,
        }
    }
}

/// Discriminator for [`JsonOutputFormat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonOutputFormatKind {
    /// Constrain to a JSON Schema.
    JsonSchema,
}

#[cfg(test)]
#[path = "../../../tests/unit/api_types_output_config.rs"]
mod tests;
