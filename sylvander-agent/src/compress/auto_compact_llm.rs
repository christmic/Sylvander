//! `AutoCompactLlm` — abstraction over the LLM call the L4
//! `AutoCompactLayer` makes to summarize a conversation.
//!
//! The trait lets us:
//! - Test the L4 layer with a mock LLM (no real API calls)
//! - Swap implementations (production uses `AgentLoopAutoCompactLlm`,
//!   tests use a closure or a recording stub)
//!
//! The signature returns `Pin<Box<dyn Future<...>>` so the trait is
//! object-safe (`&dyn AutoCompactLlm`). This mirrors the
//! `CompressionLayer` design.

use std::future::Future;
use std::pin::Pin;

use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::ModelInfo;
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::{ContentBlock, MessageParam, SystemPrompt};

use crate::error::AgentLoopError;

/// System prompt used for L4 summarization.
pub const DEFAULT_SUMMARY_PROMPT: &str = "You are a conversation compressor. \
    Given the conversation so far, produce a concise summary that preserves:\n\
    1. The original user task and any constraints they stated\n\
    2. Key tool calls and their results (file paths, IDs, conclusions)\n\
    3. Decisions made and the reasoning behind them\n\
    4. Open questions or unfinished work\n\n\
    Output plain text only — no tool calls, no markdown. Be terse. \
    The summary will replace the earlier turns in the model's context, so it \
    must contain everything the model will need to continue the task without \
    re-fetching information.";

/// Trait for the LLM call L4 makes to summarize the conversation.
pub trait AutoCompactLlm: Send + Sync {
    /// Summarize `messages` into a compact text. The returned
    /// string replaces the input messages (minus a few recent
    /// turns preserved by the layer).
    fn summarize<'a>(
        &'a self,
        messages: &'a [MessageParam],
        model: &'a ModelInfo,
    ) -> Pin<Box<dyn Future<Output = Result<String, AgentLoopError>> + Send + 'a>>;
}

/// Production LLM impl: wraps an `AnthropicClient`.
pub struct AgentLoopAutoCompactLlm {
    pub client: AnthropicClient,
    pub summary_prompt: String,
}

impl AgentLoopAutoCompactLlm {
    #[must_use]
    pub fn new(client: AnthropicClient) -> Self {
        Self {
            client,
            summary_prompt: DEFAULT_SUMMARY_PROMPT.to_string(),
        }
    }

    #[must_use]
    pub fn with_summary_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.summary_prompt = prompt.into();
        self
    }
}

impl AutoCompactLlm for AgentLoopAutoCompactLlm {
    fn summarize<'a>(
        &'a self,
        messages: &'a [MessageParam],
        model: &'a ModelInfo,
    ) -> Pin<Box<dyn Future<Output = Result<String, AgentLoopError>> + Send + 'a>> {
        Box::pin(async move {
            let req = CreateMessageRequest::builder()
                .model(model.id.clone())
                .max_tokens(4096_u32)
                .messages(messages.to_vec())
                .system(SystemPrompt::String(self.summary_prompt.clone()))
                .build()
                .map_err(|e| {
                    AgentLoopError::Compression(format!("auto_compact: build request: {e}"))
                })?;

            let response = self
                .client
                .messages()
                .create(&req)
                .await
                .map_err(|e| AgentLoopError::Compression(format!("auto_compact: {e}")))?;

            let text = response
                .content
                .iter()
                .find_map(|block| match block {
                    ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .ok_or_else(|| {
                    AgentLoopError::Compression(
                        "auto_compact: model returned no text in response".into(),
                    )
                })?;

            Ok(text)
        })
    }
}

#[cfg(test)]
pub mod tests {
    //! Test-only mock LLM exposed for downstream test modules.
    use super::*;

    /// Mock LLM that returns a canned summary, recording the input.
    #[derive(Default)]
    pub struct MockAutoCompactLlm {
        pub last_messages: std::sync::Mutex<Vec<MessageParam>>,
        pub canned_response: std::sync::Mutex<String>,
    }

    impl MockAutoCompactLlm {
        #[must_use]
        pub fn new(response: impl Into<String>) -> Self {
            Self {
                last_messages: std::sync::Mutex::new(Vec::new()),
                canned_response: std::sync::Mutex::new(response.into()),
            }
        }

        #[must_use]
        pub fn last_messages(&self) -> Vec<MessageParam> {
            self.last_messages.lock().unwrap().clone()
        }
    }

    impl AutoCompactLlm for MockAutoCompactLlm {
        fn summarize<'a>(
            &'a self,
            messages: &'a [MessageParam],
            _model: &'a ModelInfo,
        ) -> Pin<Box<dyn Future<Output = Result<String, AgentLoopError>> + Send + 'a>> {
            Box::pin(async move {
                *self.last_messages.lock().unwrap() = messages.to_vec();
                Ok(self.canned_response.lock().unwrap().clone())
            })
        }
    }
}