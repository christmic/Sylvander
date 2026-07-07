//! Tool result blocks (user-turn payloads that re-feed tool output to the model).

use serde::{Deserialize, Serialize};

use super::cache::CacheControl;
use super::image::ImageBlock;
use serde_json::Value as JsonValue;

/// User-turn content block that re-feeds a tool's output to the model.
///
/// Wire format:
/// ```json
/// {
///   "type": "tool_result",
///   "tool_use_id": "toolu_xxx",
///   "content": "the file contains: ..."
/// }
/// ```
///
/// `content` may be either a plain string or an array of structured
/// content blocks (text / image / etc.) for tool results that need to
/// return rich data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultBlock {
    /// Discriminator: always `"tool_result"`. Required by the
    /// Anthropic API; the wire format is
    /// `{"type": "tool_result", "tool_use_id": "...", ...}`.
    #[serde(rename = "type")]
    pub kind: ToolResultBlockKind,
    /// The `id` of the corresponding `tool_use` block.
    pub tool_use_id: String,
    /// The result payload — either a string or a list of structured
    /// content blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ToolResultContent>,
    /// Set to `true` when the tool failed; the model can then react
    /// accordingly.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    /// Optional cache control breakpoint at this block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// Discriminator for [`ToolResultBlock`]. Always `ToolResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolResultBlockKind {
    /// Anthropic wire value: `"tool_result"`.
    #[serde(rename = "tool_result")]
    ToolResult,
}

impl ToolResultBlock {
    /// Create a tool result block with the given `tool_use_id` and a
    /// plain-string content. (Equivalent to [`Self::new`] — kept as
    /// an alias for backwards compatibility.)
    #[must_use]
    pub fn new(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::with_string(tool_use_id, content)
    }

    /// Create a tool result block with the given `tool_use_id` and a
    /// plain-string content. Default `kind = ToolResult`,
    /// `is_error = false`.
    #[must_use]
    pub fn with_string(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            kind: ToolResultBlockKind::ToolResult,
            tool_use_id: tool_use_id.into(),
            content: Some(ToolResultContent::String(content.into())),
            is_error: false,
            cache_control: None,
        }
    }

    /// Create a tool result block with structured content blocks.
    #[must_use]
    pub fn with_blocks(tool_use_id: impl Into<String>, blocks: Vec<RichToolResultBlock>) -> Self {
        Self {
            kind: ToolResultBlockKind::ToolResult,
            tool_use_id: tool_use_id.into(),
            content: Some(ToolResultContent::Blocks(blocks)),
            is_error: false,
            cache_control: None,
        }
    }

    /// Mark this result as an error. Alias for [`Self::as_error`].
    #[must_use]
    pub fn as_error(mut self) -> Self {
        self.is_error = true;
        self
    }

    /// Attach a cache control breakpoint to this block.
    #[must_use]
    pub fn with_cache_control(mut self, cc: CacheControl) -> Self {
        self.cache_control = Some(cc);
        self
    }
}

/// Tool result content: either a plain string or a list of rich content
/// blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// Plain string content.
    String(String),
    /// Structured content blocks.
    Blocks(Vec<RichToolResultBlock>),
}

/// Structured content block nested inside a [`ToolResultContent::Blocks`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RichToolResultBlock {
    /// Text content.
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    /// Image content.
    Image(ImageBlock),
    /// Opaque JSON block (tool-specific structured output not yet
    /// strong-typed in v2).
    #[serde(untagged)]
    Other(JsonValue),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_content_round_trip() {
        let block = ToolResultBlock::new("toolu_abc", "file contents here");
        let json = serde_json::to_string(&block).unwrap();
        let back: ToolResultBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn error_marker_serializes() {
        let block = ToolResultBlock::new("toolu_abc", "permission denied").as_error();
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains(r#""is_error":true"#));
        let back: ToolResultBlock = serde_json::from_str(&json).unwrap();
        assert!(back.is_error);
    }

    #[test]
    fn cache_control_included_when_set() {
        let block = ToolResultBlock::new("toolu_abc", "x").with_cache_control(CacheControl::ephemeral());
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains(r#""cache_control":"#));
    }

    #[test]
    fn cache_control_omitted_when_none() {
        let block = ToolResultBlock::new("toolu_abc", "x");
        let json = serde_json::to_string(&block).unwrap();
        assert!(!json.contains("cache_control"));
    }

    #[test]
    fn rich_blocks_serialize_correctly() {
        let blocks = vec![RichToolResultBlock::Text {
            text: "first line".to_string(),
            cache_control: None,
        }];
        let block = ToolResultBlock::with_blocks("toolu_abc", blocks);
        let json = serde_json::to_string(&block).unwrap();
        let back: ToolResultBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, block);
    }
}