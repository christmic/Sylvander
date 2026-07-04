//! System prompt — the `system` field on a request.
//!
//! Can be a plain string or a list of [`SystemBlock`]s for cases that
//! need per-block cache control.

use serde::{Deserialize, Serialize};

use super::cache::CacheControl;

/// System prompt: either a plain string or structured blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    /// Plain string content.
    String(String),
    /// Structured system blocks (each can carry its own `cache_control`).
    Blocks(Vec<SystemBlock>),
}

impl From<&str> for SystemPrompt {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl From<String> for SystemPrompt {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

/// A single block within a structured system prompt. The `type`
/// discriminator is carried by the outer enum tag, not on this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemBlock {
    /// Text block (the only supported system block type).
    Text(SystemTextBlock),
}

/// A text system block with optional cache control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemTextBlock {
    /// The text content.
    pub text: String,
    /// Optional cache control breakpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl SystemTextBlock {
    /// Create a text system block with no cache control.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_system_round_trip() {
        let sp: SystemPrompt = "You are a helpful assistant.".into();
        let json = serde_json::to_string(&sp).unwrap();
        assert_eq!(json, r#""You are a helpful assistant.""#);
        let back: SystemPrompt = serde_json::from_str(&json).unwrap();
        assert_eq!(back, sp);
    }

    #[test]
    fn blocks_system_round_trip() {
        let sp = SystemPrompt::Blocks(vec![SystemBlock::Text(
            SystemTextBlock::new("You are a helpful assistant.")
                .with_cache_control(CacheControl::ephemeral()),
        )]);
        let json = serde_json::to_string(&sp).unwrap();
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains(r#""cache_control":"#));
        let back: SystemPrompt = serde_json::from_str(&json).unwrap();
        assert_eq!(back, sp);
    }
}