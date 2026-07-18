//! Custom function tool declarations.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::cache::CacheControl;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
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
            cache_control: None,
        }
    }

    /// Attach a cache control breakpoint to this tool.
    #[must_use]
    pub fn with_cache_control(mut self, cc: CacheControl) -> Self {
        self.cache_control = Some(cc);
        self
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
    pub fn new_with_properties(properties: JsonValue, required: &[&str]) -> Self {
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
#[path = "../../../tests/unit/api_types_tool.rs"]
mod tests;
