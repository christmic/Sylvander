//! Content blocks — the elements of message `content` arrays.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::cache::CacheControl;
use super::image::ImageBlock;
use super::tool_result::ToolResultBlock;

/// Assistant-turn content block (returned by the model).
///
/// Each variant is self-describing via its own `type` discriminator. The
/// enum is `untagged` so each block shape serializes as a flat object
/// matching the protocol's wire format directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentBlock {
    /// Plain text block.
    Text(TextBlock),
    /// Extended thinking block (model's internal reasoning).
    Thinking(ThinkingBlock),
    /// Tool use block — the model is invoking a tool.
    ToolUse(ToolUseBlock),
}

impl ContentBlock {
    /// If this is a [`ContentBlock::Text`], return its text; otherwise
    /// `None`.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(&t.text),
            _ => None,
        }
    }

    /// If this is a [`ContentBlock::ToolUse`], return the inner block.
    #[must_use]
    pub fn as_tool_use(&self) -> Option<&ToolUseBlock> {
        match self {
            Self::ToolUse(t) => Some(t),
            _ => None,
        }
    }
}

/// Text content block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextBlock {
    /// Always `"text"`.
    #[serde(rename = "type")]
    pub kind: TextBlockKind,
    /// The text content.
    pub text: String,
    /// Optional cache control breakpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl TextBlock {
    /// Create a text block with no cache control.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            kind: TextBlockKind::Text,
            text: text.into(),
            cache_control: None,
        }
    }

    /// Attach a cache control breakpoint.
    #[must_use]
    pub fn with_cache_control(mut self, cc: CacheControl) -> Self {
        self.cache_control = Some(cc);
        self
    }
}

/// Text block discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextBlockKind {
    /// Text block.
    #[serde(rename = "text")]
    Text,
}

/// Extended thinking content block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingBlock {
    /// Always `"thinking"`.
    #[serde(rename = "type")]
    pub kind: ThinkingBlockKind,
    /// The thinking text. Pass back unchanged in subsequent requests if
    /// you want to preserve the reasoning chain.
    pub thinking: String,
    /// Signature for the thinking block — required for re-feed.
    pub signature: String,
    /// Optional cache control breakpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl ThinkingBlock {
    /// Create a thinking block.
    #[must_use]
    pub fn new(thinking: impl Into<String>, signature: impl Into<String>) -> Self {
        Self {
            kind: ThinkingBlockKind::Thinking,
            thinking: thinking.into(),
            signature: signature.into(),
            cache_control: None,
        }
    }
}

/// Thinking block discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingBlockKind {
    /// Thinking block.
    #[serde(rename = "thinking")]
    Thinking,
}

/// A model's tool invocation. The handler should execute the named tool
/// with `input` and re-feed the result via [`ToolResultBlock`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUseBlock {
    /// Always `"tool_use"`.
    #[serde(rename = "type")]
    pub kind: ToolUseBlockKind,
    /// Unique identifier for this tool call. Pass back in
    /// [`ToolResultBlock::new`] to associate the result.
    pub id: String,
    /// Name of the tool to invoke.
    pub name: String,
    /// Tool input (parsed JSON object).
    pub input: JsonValue,
}

impl ToolUseBlock {
    /// Create a new tool use block.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        input: JsonValue,
    ) -> Self {
        Self {
            kind: ToolUseBlockKind::ToolUse,
            id: id.into(),
            name: name.into(),
            input,
        }
    }
}

/// Tool use block discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolUseBlockKind {
    /// Tool invocation.
    #[serde(rename = "tool_use")]
    ToolUse,
}

/// User-turn content block (sent in the request).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContentBlock {
    /// Plain text block.
    Text(TextBlock),
    /// Image content (base64 inline).
    Image(ImageBlock),
    /// Tool result re-feed.
    ToolResult(ToolResultBlock),
    /// Opaque JSON block for tool-specific structured user content
    /// (search results, document references, etc.).
    Other(JsonValue),
}

impl UserContentBlock {
    /// Create a text user content block.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextBlock::new(text))
    }

    /// Create a tool result user content block.
    #[must_use]
    pub fn tool_result(block: ToolResultBlock) -> Self {
        Self::ToolResult(block)
    }
}

/// User-message content: either a single string or an array of blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    /// Single string content (shorthand for a one-block text content).
    String(String),
    /// Array of structured content blocks.
    Blocks(Vec<UserContentBlock>),
}

impl From<&str> for UserContent {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl From<String> for UserContent {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::UserContentBlock;
    use serde_json::json;

    #[test]
    fn text_block_round_trip() {
        let block = ContentBlock::Text(TextBlock::new("Hello"));
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(json, r#"{"type":"text","text":"Hello"}"#);
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn text_block_with_cache_control() {
        let block = ContentBlock::Text(
            TextBlock::new("Hello").with_cache_control(CacheControl::ephemeral()),
        );
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains(r#""cache_control":"#));
    }

    #[test]
    fn thinking_block_round_trip() {
        let block = ContentBlock::Thinking(ThinkingBlock::new("Let me think...", "sig_xxx"));
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(
            json,
            r#"{"type":"thinking","thinking":"Let me think...","signature":"sig_xxx"}"#
        );
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn tool_use_block_bare_round_trip() {
        let block = ToolUseBlock::new(
            "toolu_abc",
            "Read",
            json!({"file_path": "/a/b.txt"}),
        );
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(
            json,
            r#"{"type":"tool_use","id":"toolu_abc","name":"Read","input":{"file_path":"/a/b.txt"}}"#
        );
        let back: ToolUseBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn tool_use_block_via_content_block_round_trip() {
        let cb = ContentBlock::ToolUse(ToolUseBlock::new(
            "toolu_abc",
            "Bash",
            json!({"command": "ls"}),
        ));
        let json = serde_json::to_string(&cb).unwrap();
        assert_eq!(
            json,
            r#"{"type":"tool_use","id":"toolu_abc","name":"Bash","input":{"command":"ls"}}"#
        );
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cb);
    }

    #[test]
    fn user_content_string_shorthand() {
        let uc = UserContent::from("hello");
        let json = serde_json::to_string(&uc).unwrap();
        assert_eq!(json, r#""hello""#);
    }

    #[test]
    fn user_content_blocks_round_trip() {
        let uc = UserContent::Blocks(vec![UserContentBlock::text("hi")]);
        let json = serde_json::to_string(&uc).unwrap();
        let back: UserContent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, uc);
    }

    #[test]
    fn content_block_text_helper() {
        let cb = ContentBlock::Text(TextBlock::new("hello"));
        assert_eq!(cb.text(), Some("hello"));
    }

    #[test]
    fn content_block_tool_use_helper() {
        let cb = ContentBlock::ToolUse(ToolUseBlock::new("x", "Bash", json!({})));
        assert!(cb.as_tool_use().is_some());
    }
}