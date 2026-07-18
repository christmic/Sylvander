//! Owned provider-neutral request, conversation, and response types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ModelRef, TokenUsage};

/// Author of one normalized conversation message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    /// End user or tool-driving caller.
    User,
    /// Model response re-fed on a later turn.
    Assistant,
}

/// One role-tagged message containing typed content blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Message author.
    pub role: ChatRole,
    /// Ordered text, reasoning, tool, or media content.
    pub content: Vec<ContentBlock>,
}

impl ChatMessage {
    #[must_use]
    /// Construct one user message containing a text block.
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
    /// Provider registry kind that owns the payload.
    pub provider: String,
    /// Opaque provider wire state persisted without interpretation.
    pub data: Value,
}

/// Location and encoding of binary model input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaSource {
    /// Inline base64 payload with an explicit media type.
    Base64 {
        /// MIME-like media type.
        media_type: String,
        /// Base64-encoded payload.
        data: String,
    },
    /// Provider-readable URL.
    Url {
        /// Absolute media URL.
        url: String,
    },
}

/// Image input and optional accessible description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContent {
    /// Image location or inline data.
    pub source: MediaSource,
    /// Optional human-readable alternative text.
    pub alt_text: Option<String>,
}

/// Document input and optional display title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentContent {
    /// Document location or inline data.
    pub source: MediaSource,
    /// Optional document title.
    pub title: Option<String>,
}

/// Typed content returned by a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContent {
    /// Textual tool output.
    Text {
        /// Tool result text.
        text: String,
    },
    /// Image tool output.
    Image {
        /// Typed image content.
        image: ImageContent,
    },
    /// Document tool output.
    Document {
        /// Typed document content.
        document: DocumentContent,
    },
}

/// Provider-neutral conversation content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// User-visible or assistant text.
    Text {
        /// Text content.
        text: String,
    },
    /// Model reasoning plus optional provider-owned replay state.
    Reasoning {
        /// Reasoning text as supplied by the adapter.
        text: String,
        /// Opaque state required to re-feed the reasoning block.
        opaque_state: Option<OpaqueProviderState>,
    },
    /// Model request to invoke one tool.
    ToolCall {
        /// Provider-stable call identifier.
        id: String,
        /// Registered tool name.
        name: String,
        /// JSON arguments validated by the tool boundary.
        arguments: Value,
    },
    /// Result of a previous tool invocation.
    ToolResult {
        /// Identifier of the matching tool call.
        call_id: String,
        /// Typed result items.
        content: Vec<ToolResultContent>,
        /// Whether tool execution failed.
        is_error: bool,
    },
    /// Direct image input.
    Image {
        /// Typed image content.
        image: ImageContent,
    },
    /// Direct document input.
    Document {
        /// Typed document content.
        document: DocumentContent,
    },
}

/// Provider-neutral prompt cache directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheHint {
    /// Cache the marked prefix under provider-defined ephemeral semantics.
    Ephemeral,
}

/// One ordered system instruction and optional cache boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemInstruction {
    /// Instruction text.
    pub text: String,
    /// Optional cache directive.
    pub cache_hint: Option<CacheHint>,
}

/// Tool schema advertised to a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Stable registered tool name.
    pub name: String,
    /// Model-facing tool description.
    pub description: String,
    /// JSON Schema for call arguments.
    pub input_schema: Value,
    /// Optional cache directive.
    pub cache_hint: Option<CacheHint>,
}

/// Explicit reasoning-token budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningConfig {
    /// Maximum tokens reserved for reasoning.
    pub budget_tokens: u32,
}

/// Complete provider-neutral model invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRequest {
    /// Runtime correlation identifier.
    pub request_id: String,
    /// Provider-qualified selected model.
    pub model: ModelRef,
    /// Ordered system instructions.
    pub system: Vec<SystemInstruction>,
    /// Conversation history.
    pub messages: Vec<ChatMessage>,
    /// Tool definitions available for this invocation.
    pub tools: Vec<ToolDefinition>,
    /// Maximum generated tokens.
    pub max_output_tokens: u32,
    /// Optional reasoning budget.
    pub reasoning: Option<ReasoningConfig>,
    /// Optional JSON Schema constraining assistant output.
    pub output_schema: Option<Value>,
}

/// Normalized reason a model stopped generating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum StopReason {
    /// Model completed the assistant turn.
    EndTurn,
    /// Model stopped to request tool execution.
    ToolUse,
    /// Configured output-token limit was reached.
    MaxOutputTokens,
    /// Configured stop sequence was emitted.
    StopSequence(String),
    /// Provider classified the response as a refusal.
    Refusal,
    /// Provider paused a resumable response.
    Paused,
    /// Provider-specific stop reason retained as text.
    Other(String),
}

/// Terminal normalized model response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelResponse {
    /// Provider response identifier.
    pub id: String,
    /// Provider-qualified model that produced the response.
    pub model: ModelRef,
    /// Ordered response blocks.
    pub content: Vec<ContentBlock>,
    /// Normalized terminal reason.
    pub stop_reason: StopReason,
    /// Provider-reported token usage.
    pub usage: TokenUsage,
}

/// Incremental provider-neutral stream item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelStreamEvent {
    /// Incremental assistant text.
    TextDelta(String),
    /// Incremental model reasoning.
    ReasoningDelta(String),
    /// Exactly one terminal assembled response.
    Completed(ModelResponse),
}
