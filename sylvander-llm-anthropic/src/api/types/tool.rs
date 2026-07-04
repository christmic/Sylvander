//! Custom function tool declarations.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// A custom function tool that the model may invoke.
///
/// Wire format:
/// ```json
/// {
///   "name": "get_weather",
///   "description": "Get current weather for a location",
///   "input_schema": {
///     "type": "object",
///     "properties": { "location": { "type": "string" } },
///     "required": ["location"]
///   }
/// }
/// ```
///
/// Sylvander v2 supports **custom function tools only** — built-in tools
/// like `bash` / `text_editor` / `web_search` are not exposed at this layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    /// Tool name (must be unique within the request). The model will
    /// generate this name in `tool_use` blocks.
    pub name: String,
    /// Human-readable description; the model uses this to decide when to
    /// invoke the tool.
    pub description: String,
    /// JSON Schema describing the tool's input parameters.
    pub input_schema: InputSchema,
}

impl Tool {
    /// Create a new tool with the given name, description, and input schema.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: InputSchema,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// JSON Schema for a tool's input. Defaults to an empty object schema that
/// accepts no parameters.
///
/// Use [`InputSchema::new_with_properties`] to build a more interesting
/// schema, or [`InputSchema::from_json_value`] to construct from an
/// existing JSON Schema value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSchema {
    /// The JSON Schema as a value. Always `"object"` for tool inputs.
    #[serde(flatten)]
    pub schema: JsonValue,
}

impl Default for InputSchema {
    fn default() -> Self {
        Self {
            schema: serde_json::json!({"type": "object"}),
        }
    }
}

impl InputSchema {
    /// Empty object schema — the tool takes no parameters.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create an input schema from a JSON Schema value.
    #[must_use]
    pub fn from_json_value(schema: JsonValue) -> Self {
        Self { schema }
    }

    /// Build a simple object schema with the given properties and required
    /// list.
    #[must_use]
    pub fn new_with_properties(
        properties: JsonValue,
        required: &[&str],
    ) -> Self {
        Self {
            schema: serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": required,
            }),
        }
    }
}

/// How the model should choose a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    /// Model decides whether to invoke any tools. (default if omitted)
    Auto,
    /// Model must invoke at least one tool.
    Any,
    /// Model must not invoke any tools.
    None,
    /// Model must invoke the named tool.
    Tool {
        /// Name of the tool to invoke.
        name: String,
        /// When `true`, disallows the model from invoking multiple tools in
        /// parallel in the same turn.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        disable_parallel_tool_use: bool,
    },
}

impl ToolChoice {
    /// `auto` tool choice.
    #[must_use]
    pub const fn auto() -> Self {
        Self::Auto
    }

    /// `any` tool choice.
    #[must_use]
    pub const fn any() -> Self {
        Self::Any
    }

    /// `none` tool choice.
    #[must_use]
    pub const fn none() -> Self {
        Self::None
    }

    /// Specific tool choice with parallel tool use enabled.
    #[must_use]
    pub fn tool(name: impl Into<String>) -> Self {
        Self::Tool {
            name: name.into(),
            disable_parallel_tool_use: false,
        }
    }

    /// Specific tool choice with parallel tool use disabled.
    #[must_use]
    pub fn tool_serial(name: impl Into<String>) -> Self {
        Self::Tool {
            name: name.into(),
            disable_parallel_tool_use: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_basic_wire_format() {
        let tool = Tool::new(
            "get_weather",
            "Get current weather for a location",
            InputSchema::new_with_properties(
                json!({"location": {"type": "string"}}),
                &["location"],
            ),
        );
        let json = serde_json::to_string(&tool).unwrap();
        let back: Tool = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tool);
    }

    #[test]
    fn tool_empty_schema_wire_format() {
        let tool = Tool::new("ping", "Health check", InputSchema::empty());
        let json = serde_json::to_string(&tool).unwrap();
        assert_eq!(
            json,
            r#"{"name":"ping","description":"Health check","input_schema":{"type":"object"}}"#
        );
    }

    #[test]
    fn tool_choice_auto() {
        assert_eq!(
            serde_json::to_string(&ToolChoice::auto()).unwrap(),
            r#"{"type":"auto"}"#
        );
    }

    #[test]
    fn tool_choice_any() {
        assert_eq!(
            serde_json::to_string(&ToolChoice::any()).unwrap(),
            r#"{"type":"any"}"#
        );
    }

    #[test]
    fn tool_choice_none() {
        assert_eq!(
            serde_json::to_string(&ToolChoice::none()).unwrap(),
            r#"{"type":"none"}"#
        );
    }

    #[test]
    fn tool_choice_specific_serializes_correctly() {
        let tc = ToolChoice::tool_serial("Read");
        assert_eq!(
            serde_json::to_string(&tc).unwrap(),
            r#"{"type":"tool","name":"Read","disable_parallel_tool_use":true}"#
        );
    }

    #[test]
    fn tool_choice_specific_omits_disable_parallel_when_false() {
        let tc = ToolChoice::tool("Read");
        assert_eq!(
            serde_json::to_string(&tc).unwrap(),
            r#"{"type":"tool","name":"Read"}"#
        );
    }

    #[test]
    fn tool_choice_round_trip() {
        for tc in [
            ToolChoice::auto(),
            ToolChoice::any(),
            ToolChoice::none(),
            ToolChoice::tool("Read"),
            ToolChoice::tool_serial("Bash"),
        ] {
            let s = serde_json::to_string(&tc).unwrap();
            let back: ToolChoice = serde_json::from_str(&s).unwrap();
            assert_eq!(back, tc);
        }
    }
}