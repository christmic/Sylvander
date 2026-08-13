//! Native `DashScope` Generation request and response wire types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenerationRequest {
    pub model: String,
    pub input: GenerationInput,
    pub parameters: GenerationParameters,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MultimodalGenerationRequest {
    pub model: String,
    pub input: MultimodalGenerationInput,
    pub parameters: GenerationParameters,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MultimodalGenerationInput {
    pub messages: Vec<MultimodalMessageParam>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MultimodalMessageParam {
    pub role: String,
    pub content: Vec<MultimodalContent>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum MultimodalContent {
    Text { text: String },
    Image { image: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenerationInput {
    pub messages: Vec<GenerationMessageParam>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum GenerationMessageParam {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<GenerationToolCallParam>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenerationToolCallParam {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: GenerationToolKind,
    pub function: GenerationFunctionCallParam,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GenerationToolKind {
    Function,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenerationFunctionCallParam {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenerationFunctionTool {
    #[serde(rename = "type")]
    pub kind: GenerationToolKind,
    pub function: GenerationFunctionDefinition,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenerationFunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenerationParameters {
    pub result_format: String,
    pub incremental_output: bool,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<GenerationFunctionTool>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GenerationResponse {
    pub request_id: Option<String>,
    pub output: Option<GenerationOutput>,
    pub usage: Option<GenerationUsage>,
    pub code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GenerationOutput {
    pub text: Option<String>,
    #[serde(default)]
    pub choices: Vec<GenerationChoice>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GenerationChoice {
    pub index: Option<u64>,
    pub message: Option<GenerationMessage>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GenerationMessage {
    pub content: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct GenerationUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: Option<u64>,
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    pub output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct PromptTokensDetails {
    pub cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct OutputTokensDetails {
    pub reasoning_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenerationCompletion {
    pub request_id: String,
    pub content: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<GenerationToolCall>,
    pub finish_reason: String,
    pub usage: GenerationUsage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenerationToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}
