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
    Low,
    Medium,
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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_effort_only() {
        let oc = OutputConfig {
            effort: Some(Effort::High),
            format: None,
        };
        assert_eq!(
            serde_json::to_string(&oc).unwrap(),
            r#"{"effort":"high"}"#
        );
    }

    #[test]
    fn serializes_format_only() {
        let schema = json!({"type": "object", "properties": {"answer": {"type": "string"}}});
        let oc = OutputConfig {
            effort: None,
            format: Some(JsonOutputFormat::new(schema.clone())),
        };
        let json = serde_json::to_string(&oc).unwrap();
        let back: OutputConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.format.unwrap().schema, schema);
    }

    #[test]
    fn serializes_empty_omits_fields() {
        let oc = OutputConfig::default();
        assert_eq!(serde_json::to_string(&oc).unwrap(), r"{}");
    }

    #[test]
    fn json_output_format_wire_format() {
        let schema = json!({"type": "object"});
        let f = JsonOutputFormat::new(schema.clone());
        let json = serde_json::to_string(&f).unwrap();
        let back: JsonOutputFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, JsonOutputFormatKind::JsonSchema);
        assert_eq!(back.schema, schema);
    }
}