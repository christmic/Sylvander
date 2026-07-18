//! `AutoCompactLlm` — abstraction over the LLM call the L4
//! `AutoCompactLayer` makes to summarize a conversation.
//!
//! The trait lets us:
//! - Test the L4 layer with a mock LLM (no real API calls)
//! - Swap implementations (the loop uses an internal backend adapter;
//!   tests use a closure or a recording stub)
//!
//! The signature returns `Pin<Box<dyn Future<...>>` so the trait is
//! object-safe (`&dyn AutoCompactLlm`). This mirrors the
//! `CompressionLayer` design.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::StreamExt as _;
use sylvander_llm_anthropic::api::model::ModelInfo;
use sylvander_llm_anthropic::api::types::MessageParam;

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

pub(crate) struct ProviderAutoCompactLlm {
    provider: Arc<dyn sylvander_llm_core::ModelProvider>,
    model: sylvander_llm_core::ModelInfo,
}

impl ProviderAutoCompactLlm {
    pub(crate) fn new(
        provider: Arc<dyn sylvander_llm_core::ModelProvider>,
        model: sylvander_llm_core::ModelInfo,
    ) -> Self {
        Self { provider, model }
    }
}

impl AutoCompactLlm for ProviderAutoCompactLlm {
    fn summarize<'a>(
        &'a self,
        messages: &'a [MessageParam],
        _model: &'a ModelInfo,
    ) -> Pin<Box<dyn Future<Output = Result<String, AgentLoopError>> + Send + 'a>> {
        Box::pin(async move {
            let messages = messages
                .iter()
                .map(crate::provider_adapter::message_to_core)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| AgentLoopError::Compression(error.to_string()))?;
            let request = sylvander_llm_core::ModelRequest {
                request_id: uuid::Uuid::new_v4().to_string(),
                model: self.model.reference.clone(),
                system: vec![sylvander_llm_core::SystemInstruction {
                    text: DEFAULT_SUMMARY_PROMPT.into(),
                    cache_hint: None,
                }],
                messages,
                tools: Vec::new(),
                max_output_tokens: self.model.max_output_tokens.min(4096),
                reasoning: None,
                output_schema: None,
            };
            let mut stream = self
                .provider
                .complete_stream(request)
                .await
                .map_err(provider_error)?;
            let mut completed = None;
            while let Some(event) = stream.next().await {
                let event = event.map_err(provider_error)?;
                if completed.is_some() {
                    return Err(protocol(
                        "summary provider emitted an event after completion",
                    ));
                }
                match event {
                    sylvander_llm_core::ModelStreamEvent::TextDelta(_) => {}
                    sylvander_llm_core::ModelStreamEvent::ReasoningDelta(_) => {
                        return Err(protocol("summary provider emitted reasoning"));
                    }
                    sylvander_llm_core::ModelStreamEvent::Completed(response) => {
                        if response.model != self.model.reference {
                            return Err(protocol("summary provider returned an unexpected model"));
                        }
                        completed = Some(response);
                    }
                }
            }
            let response =
                completed.ok_or_else(|| protocol("summary stream ended without completion"))?;
            let mut text = String::new();
            for block in response.content {
                let sylvander_llm_core::ContentBlock::Text { text: part } = block else {
                    return Err(protocol("summary response contained non-text content"));
                };
                text.push_str(&part);
            }
            if text.trim().is_empty() {
                return Err(protocol("summary response contained no text"));
            }
            Ok(text)
        })
    }
}

fn provider_error(source: sylvander_llm_core::ProviderError) -> AgentLoopError {
    AgentLoopError::Provider {
        attempts: 1,
        source,
    }
}

fn protocol(message: &'static str) -> AgentLoopError {
    provider_error(sylvander_llm_core::ProviderError::new(
        sylvander_llm_core::ProviderErrorKind::Protocol,
        sylvander_llm_core::ProviderErrorPhase::Stream,
        message,
    ))
}

#[cfg(test)]
#[path = "../../tests/unit/compress_auto_compact_llm.rs"]
pub mod tests;
