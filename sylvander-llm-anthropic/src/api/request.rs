//! Request types — `CreateMessageRequest` is the input for both
//! `POST /v1/messages` (create / stream) and `POST /v1/messages/count_tokens`.

use serde::{Deserialize, Serialize};

use super::types::{MessageParam, OutputConfig, SystemPrompt, ThinkingConfig, Tool, ToolChoice};

/// Input for `POST /v1/messages` and `POST /v1/messages/count_tokens`.
///
/// Construct via [`CreateMessageRequest::builder`]. The struct is fully
/// `Serialize` so it can be sent as the JSON body of either endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMessageRequest {
    /// Model to invoke. Always present (no default).
    pub model: String,

    /// Maximum tokens to generate. Must be `> 0`; setting it to `0` only
    /// pre-warms the prompt cache without generating output.
    pub max_tokens: u32,

    /// Input messages. Must contain at least one message. For
    /// `count_tokens` requests, the model never sees this — only used for
    /// token estimation.
    pub messages: Vec<MessageParam>,

    /// Optional system prompt. Can be a plain string or a list of text
    /// blocks (with per-block `cache_control`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemPrompt>,

    /// Custom function tools the model may invoke.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,

    /// How the model should choose a tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,

    /// Extended thinking configuration. When present, the client
    /// auto-attaches the `extended-thinking-2025-01-01` beta header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,

    /// Structured output schema constraint. When present, the client
    /// auto-attaches the `structured-outputs-2025-06-01` beta header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,

    /// Sampling temperature (0.0–1.0, recommended).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Nucleus sampling parameter (0.0–1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// Top-K sampling parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,

    /// Custom stop sequences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
}

impl CreateMessageRequest {
    /// Start building a request.
    #[must_use]
    pub fn builder() -> CreateMessageRequestBuilder {
        CreateMessageRequestBuilder::default()
    }

    /// Validate the request before sending. Catches errors that the API
    /// would otherwise reject with a 4xx response.
    ///
    /// # Errors
    /// Returns [`super::error::AnthropicError::Validation`] if:
    /// - `messages` is empty
    /// - `temperature` is outside `[0.0, 1.0]`
    /// - `top_p` is outside `[0.0, 1.0]`
    /// - `thinking.budget_tokens > max_tokens`
    pub fn validate(&self) -> Result<(), super::error::AnthropicError> {
        if self.messages.is_empty() {
            return Err(super::error::AnthropicError::Validation(
                "messages must not be empty".into(),
            ));
        }
        if let Some(temp) = self.temperature
            && !(0.0..=1.0).contains(&temp)
        {
            return Err(super::error::AnthropicError::Validation(format!(
                "temperature {temp} out of range [0.0, 1.0]"
            )));
        }
        if let Some(top_p) = self.top_p
            && !(0.0..=1.0).contains(&top_p)
        {
            return Err(super::error::AnthropicError::Validation(format!(
                "top_p {top_p} out of range [0.0, 1.0]"
            )));
        }
        if let Some(thinking) = self.thinking
            && thinking.budget_tokens > self.max_tokens
        {
            return Err(super::error::AnthropicError::Validation(format!(
                "thinking.budget_tokens ({}) must be <= max_tokens ({})",
                thinking.budget_tokens, self.max_tokens
            )));
        }
        Ok(())
    }
}

/// Builder for [`CreateMessageRequest`].
#[derive(Debug, Default, Clone)]
pub struct CreateMessageRequestBuilder {
    model: Option<String>,
    max_tokens: Option<u32>,
    messages: Option<Vec<MessageParam>>,
    system: Option<SystemPrompt>,
    tools: Vec<Tool>,
    tool_choice: Option<ToolChoice>,
    thinking: Option<ThinkingConfig>,
    output_config: Option<OutputConfig>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    top_k: Option<u32>,
    stop_sequences: Vec<String>,
}

impl CreateMessageRequestBuilder {
    /// Set the model (canonical Anthropic ID string, e.g.,
    /// `"claude-sonnet-5-20260601"`).
    ///
    /// The SDK treats the model ID as opaque — caller is responsible
    /// for ensuring the model exists in their own registry.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the maximum tokens to generate.
    #[must_use]
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    /// Set the input messages.
    #[must_use]
    pub fn messages(mut self, msgs: Vec<MessageParam>) -> Self {
        self.messages = Some(msgs);
        self
    }

    /// Add a single user message.
    #[must_use]
    pub fn user_message(mut self, text: impl Into<String>) -> Self {
        let msg = MessageParam::user(text.into());
        self.messages.get_or_insert_with(Vec::new).push(msg);
        self
    }

    /// Set the system prompt.
    #[must_use]
    pub fn system(mut self, system: SystemPrompt) -> Self {
        self.system = Some(system);
        self
    }

    /// Add a tool.
    #[must_use]
    pub fn tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Add multiple tools.
    #[must_use]
    pub fn tools(mut self, tools: impl IntoIterator<Item = Tool>) -> Self {
        self.tools.extend(tools);
        self
    }

    /// Set tool choice.
    #[must_use]
    pub fn tool_choice(mut self, choice: ToolChoice) -> Self {
        self.tool_choice = Some(choice);
        self
    }

    /// Enable extended thinking with the given token budget.
    #[must_use]
    pub fn thinking(mut self, budget_tokens: u32) -> Self {
        self.thinking = Some(ThinkingConfig::new(budget_tokens));
        self
    }

    /// Set structured output configuration.
    #[must_use]
    pub fn output_config(mut self, oc: OutputConfig) -> Self {
        self.output_config = Some(oc);
        self
    }

    /// Set sampling temperature.
    #[must_use]
    pub fn temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    /// Set top-p.
    #[must_use]
    pub fn top_p(mut self, p: f32) -> Self {
        self.top_p = Some(p);
        self
    }

    /// Set top-k.
    #[must_use]
    pub fn top_k(mut self, k: u32) -> Self {
        self.top_k = Some(k);
        self
    }

    /// Add a stop sequence.
    #[must_use]
    pub fn stop_sequence(mut self, seq: impl Into<String>) -> Self {
        self.stop_sequences.push(seq.into());
        self
    }

    /// Build the request, validating that required fields are set.
    ///
    /// # Errors
    /// Returns [`super::error::AnthropicError::Validation`] if `model`
    /// or `max_tokens` is missing.
    pub fn build(self) -> Result<CreateMessageRequest, super::error::AnthropicError> {
        Ok(CreateMessageRequest {
            model: self.model.ok_or_else(|| {
                super::error::AnthropicError::Validation("model is required".into())
            })?,
            max_tokens: self.max_tokens.ok_or_else(|| {
                super::error::AnthropicError::Validation("max_tokens is required".into())
            })?,
            messages: self.messages.ok_or_else(|| {
                super::error::AnthropicError::Validation("messages is required".into())
            })?,
            system: self.system,
            tools: self.tools,
            tool_choice: self.tool_choice,
            thinking: self.thinking,
            output_config: self.output_config,
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            stop_sequences: self.stop_sequences,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::error::AnthropicError;
    use crate::api::types::{InputSchema, Tool};

    #[test]
    fn builder_minimal_required() {
        let req = CreateMessageRequest::builder()
            .model("claude-sonnet-5-20260601")
            .max_tokens(1024)
            .messages(vec![MessageParam::user("Hi")])
            .build()
            .expect("build should succeed");
        assert_eq!(req.model, "claude-sonnet-5-20260601");
        assert_eq!(req.max_tokens, 1024);
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn builder_missing_model_errors() {
        let result = CreateMessageRequest::builder()
            .max_tokens(1024)
            .messages(vec![MessageParam::user("Hi")])
            .build();
        assert!(matches!(result, Err(AnthropicError::Validation(_))));
    }

    #[test]
    fn builder_missing_max_tokens_errors() {
        let result = CreateMessageRequest::builder()
            .model("claude-sonnet-5-20260601")
            .messages(vec![MessageParam::user("Hi")])
            .build();
        assert!(matches!(result, Err(AnthropicError::Validation(_))));
    }

    #[test]
    fn builder_missing_messages_errors() {
        let result = CreateMessageRequest::builder()
            .model("claude-sonnet-5-20260601")
            .max_tokens(1024)
            .build();
        assert!(matches!(result, Err(AnthropicError::Validation(_))));
    }

    #[test]
    fn validate_empty_messages_errors() {
        let req = CreateMessageRequest::builder()
            .model("claude-sonnet-5-20260601")
            .max_tokens(1024)
            .messages(vec![])
            .build()
            .unwrap();
        assert!(req.validate().is_err());
    }

    #[test]
    fn validate_temperature_out_of_range_errors() {
        let req = CreateMessageRequest::builder()
            .model("claude-sonnet-5-20260601")
            .max_tokens(1024)
            .messages(vec![MessageParam::user("Hi")])
            .temperature(1.5)
            .build()
            .unwrap();
        assert!(req.validate().is_err());
    }

    #[test]
    fn validate_thinking_budget_greater_than_max_tokens_errors() {
        let req = CreateMessageRequest::builder()
            .model("claude-sonnet-5-20260601")
            .max_tokens(100)
            .messages(vec![MessageParam::user("Hi")])
            .thinking(200)
            .build()
            .unwrap();
        assert!(req.validate().is_err());
    }

    #[test]
    fn builder_with_tools() {
        let tool = Tool::new("ping", "Health check", InputSchema::empty());
        let req = CreateMessageRequest::builder()
            .model("claude-sonnet-5-20260601")
            .max_tokens(1024)
            .messages(vec![MessageParam::user("Ping")])
            .tool(tool)
            .build()
            .unwrap();
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "ping");
    }

    #[test]
    fn serialization_omits_optional_fields() {
        let req = CreateMessageRequest::builder()
            .model("claude-sonnet-5-20260601")
            .max_tokens(1024)
            .messages(vec![MessageParam::user("Hi")])
            .build()
            .unwrap();
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("system"));
        assert!(!json.contains("tools"));
        assert!(!json.contains("temperature"));
    }
}
