//! Owned provider-neutral request, conversation, and response types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ModelRef, TokenUsage};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: Vec<ContentBlock>,
}

impl ChatMessage {
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}

/// Provider-owned state that must survive response re-feeding.
///
/// Core persists and returns this value but never interprets its contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpaqueProviderState {
    pub provider: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContent {
    pub source: MediaSource,
    pub alt_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentContent {
    pub source: MediaSource,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContent {
    Text { text: String },
    Image { image: ImageContent },
    Document { document: DocumentContent },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
        opaque_state: Option<OpaqueProviderState>,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        content: Vec<ToolResultContent>,
        is_error: bool,
    },
    Image {
        image: ImageContent,
    },
    Document {
        document: DocumentContent,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheHint {
    Ephemeral,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemInstruction {
    pub text: String,
    pub cache_hint: Option<CacheHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub cache_hint: Option<CacheHint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningConfig {
    pub budget_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub request_id: String,
    pub model: ModelRef,
    pub system: Vec<SystemInstruction>,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDefinition>,
    pub max_output_tokens: u32,
    pub reasoning: Option<ReasoningConfig>,
    pub output_schema: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxOutputTokens,
    StopSequence(String),
    Refusal,
    Paused,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub id: String,
    pub model: ModelRef,
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelStreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
    Completed(ModelResponse),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn image() -> ImageContent {
        ImageContent {
            source: MediaSource::Base64 {
                media_type: "image/png".into(),
                data: "cG5n".into(),
            },
            alt_text: Some("diagram".into()),
        }
    }

    fn document() -> DocumentContent {
        DocumentContent {
            source: MediaSource::Url {
                url: "https://example.invalid/spec.pdf".into(),
            },
            title: Some("spec".into()),
        }
    }

    #[test]
    fn rich_request_and_response_round_trip_without_provider_wire_types() {
        let request = ModelRequest {
            request_id: "req-1".into(),
            model: ModelRef::new("provider-a", "model-a"),
            system: vec![SystemInstruction {
                text: "be precise".into(),
                cache_hint: Some(CacheHint::Ephemeral),
            }],
            messages: vec![
                ChatMessage::user("inspect this"),
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: vec![
                        ContentBlock::Reasoning {
                            text: "private reasoning".into(),
                            opaque_state: Some(OpaqueProviderState {
                                provider: "provider-a".into(),
                                data: json!({"signed": "opaque"}),
                            }),
                        },
                        ContentBlock::ToolCall {
                            id: "call-1".into(),
                            name: "read".into(),
                            arguments: json!({"path": "/tmp/a"}),
                        },
                    ],
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: vec![ContentBlock::ToolResult {
                        call_id: "call-1".into(),
                        content: vec![
                            ToolResultContent::Text {
                                text: "done".into(),
                            },
                            ToolResultContent::Image { image: image() },
                            ToolResultContent::Document {
                                document: document(),
                            },
                        ],
                        is_error: false,
                    }],
                },
            ],
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
                cache_hint: Some(CacheHint::Ephemeral),
            }],
            max_output_tokens: 4096,
            reasoning: Some(ReasoningConfig { budget_tokens: 512 }),
            output_schema: Some(json!({"type": "object"})),
        };
        let request_json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<ModelRequest>(&request_json).unwrap(),
            request
        );

        let response = ModelResponse {
            id: "message-1".into(),
            model: request.model.clone(),
            content: vec![
                ContentBlock::Text {
                    text: "done".into(),
                },
                ContentBlock::Image { image: image() },
                ContentBlock::Document {
                    document: document(),
                },
            ],
            stop_reason: StopReason::StopSequence("END".into()),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 2,
                cache_write_tokens: Some(3),
                cache_read_tokens: Some(4),
            },
        };
        let event = ModelStreamEvent::Completed(response);
        let event_json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            serde_json::from_str::<ModelStreamEvent>(&event_json).unwrap(),
            event
        );
    }
}
