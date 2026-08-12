//! `OpenAI` Chat Completions wire types used by the streaming adapter.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CreateChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessageParam>,
    pub stream: bool,
    pub stream_options: ChatStreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ChatFunctionTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ChatResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "role")]
pub enum ChatMessageParam {
    #[serde(rename = "system")]
    System { content: String },
    #[serde(rename = "user")]
    User { content: Vec<ChatUserContentPart> },
    #[serde(rename = "assistant")]
    Assistant {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ChatToolCallParam>,
    },
    #[serde(rename = "tool")]
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum ChatUserContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ChatImageUrl },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatImageUrl {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatToolCallParam {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatFunction,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatFunctionTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatFunctionDefinition,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatFunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub strict: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatResponseFormat {
    #[serde(rename = "type")]
    pub kind: String,
    pub json_schema: ChatJsonSchema,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatJsonSchema {
    pub name: String,
    pub schema: Value,
    pub strict: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ChatStreamOptions {
    pub include_usage: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub choices: Vec<ChunkChoice>,
    pub usage: Option<ChatCompletionUsage>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChunkChoice {
    pub index: u64,
    pub delta: ChoiceDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChoiceDelta {
    pub content: Option<String>,
    pub refusal: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolCallDelta {
    pub index: u64,
    pub id: Option<String>,
    pub function: Option<FunctionDelta>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FunctionDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatCompletion {
    pub id: String,
    pub model: String,
    pub content: String,
    pub refusal: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<ChatToolCall>,
    pub finish_reason: String,
    pub usage: Option<ChatCompletionUsage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct ChatCompletionUsage {
    pub completion_tokens: u64,
    pub prompt_tokens: u64,
    pub total_tokens: u64,
    pub completion_tokens_details: Option<CompletionTokensDetails>,
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "field names mirror the official OpenAI usage wire type"
)]
pub struct CompletionTokensDetails {
    pub accepted_prediction_tokens: Option<u64>,
    pub audio_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub rejected_prediction_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "field names mirror the official OpenAI usage wire type"
)]
pub struct PromptTokensDetails {
    pub audio_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
}
