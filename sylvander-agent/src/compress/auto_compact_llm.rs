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
use std::sync::Arc;

use futures_util::StreamExt as _;
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

pub(crate) enum BackendAutoCompactLlm {
    Legacy(AgentLoopAutoCompactLlm),
    Provider(ProviderAutoCompactLlm),
}

impl AutoCompactLlm for BackendAutoCompactLlm {
    fn summarize<'a>(
        &'a self,
        messages: &'a [MessageParam],
        model: &'a ModelInfo,
    ) -> Pin<Box<dyn Future<Output = Result<String, AgentLoopError>> + Send + 'a>> {
        match self {
            Self::Legacy(llm) => llm.summarize(messages, model),
            Self::Provider(llm) => llm.summarize(messages, model),
        }
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
                .map(crate::provider_compat::message_to_core)
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

    struct FakeProvider {
        events: std::sync::Mutex<
            Option<
                Vec<
                    Result<sylvander_llm_core::ModelStreamEvent, sylvander_llm_core::ProviderError>,
                >,
            >,
        >,
        request: std::sync::Mutex<Option<sylvander_llm_core::ModelRequest>>,
    }

    impl sylvander_llm_core::ModelProvider for FakeProvider {
        fn complete_stream(
            &self,
            request: sylvander_llm_core::ModelRequest,
        ) -> sylvander_llm_core::ProviderFuture<'_> {
            *self.request.lock().unwrap() = Some(request);
            let events = self.events.lock().unwrap().take().unwrap();
            Box::pin(async move {
                Ok(Box::pin(futures_util::stream::iter(events))
                    as sylvander_llm_core::ModelEventStream)
            })
        }
    }

    fn response(
        content: Vec<sylvander_llm_core::ContentBlock>,
    ) -> sylvander_llm_core::ModelResponse {
        sylvander_llm_core::ModelResponse {
            id: "summary".into(),
            model: sylvander_llm_core::ModelRef::new("local", "model-a"),
            content,
            stop_reason: sylvander_llm_core::StopReason::EndTurn,
            usage: sylvander_llm_core::TokenUsage::default(),
        }
    }

    fn provider(
        events: Vec<
            Result<sylvander_llm_core::ModelStreamEvent, sylvander_llm_core::ProviderError>,
        >,
    ) -> Arc<FakeProvider> {
        Arc::new(FakeProvider {
            events: std::sync::Mutex::new(Some(events)),
            request: std::sync::Mutex::new(None),
        })
    }

    fn provider_model() -> sylvander_llm_core::ModelInfo {
        sylvander_llm_core::ModelInfo {
            reference: sylvander_llm_core::ModelRef::new("local", "model-a"),
            context_window: 100_000,
            max_output_tokens: 8192,
            capabilities: sylvander_llm_core::ModelCapabilities::empty(),
        }
    }

    #[tokio::test]
    async fn provider_summary_is_qualified_text_only_and_minimal() {
        let provider = provider(vec![Ok(sylvander_llm_core::ModelStreamEvent::Completed(
            response(vec![sylvander_llm_core::ContentBlock::Text {
                text: "summary".into(),
            }]),
        ))]);
        let llm = ProviderAutoCompactLlm::new(provider.clone(), provider_model());
        let legacy_model = crate::provider_compat::model_metadata_from_core(&provider_model());
        let summary = llm
            .summarize(&[MessageParam::user("old context")], &legacy_model)
            .await
            .unwrap();
        assert_eq!(summary, "summary");
        let request = provider.request.lock().unwrap();
        let request = request.as_ref().unwrap();
        assert_eq!(
            request.model,
            sylvander_llm_core::ModelRef::new("local", "model-a")
        );
        assert!(
            request.tools.is_empty()
                && request.reasoning.is_none()
                && request.output_schema.is_none()
        );
        assert_eq!(request.max_output_tokens, 4096);
    }

    #[tokio::test]
    async fn provider_summary_rejects_missing_late_and_non_text_completion() {
        let cases = [
            Vec::new(),
            vec![
                Ok(sylvander_llm_core::ModelStreamEvent::Completed(response(
                    vec![sylvander_llm_core::ContentBlock::Text { text: "ok".into() }],
                ))),
                Ok(sylvander_llm_core::ModelStreamEvent::TextDelta(
                    "late".into(),
                )),
            ],
            vec![Ok(sylvander_llm_core::ModelStreamEvent::Completed(
                response(vec![sylvander_llm_core::ContentBlock::ToolCall {
                    id: "call".into(),
                    name: "tool".into(),
                    arguments: serde_json::json!({}),
                }]),
            ))],
            vec![
                Ok(sylvander_llm_core::ModelStreamEvent::Completed(response(
                    vec![sylvander_llm_core::ContentBlock::Text { text: "one".into() }],
                ))),
                Ok(sylvander_llm_core::ModelStreamEvent::Completed(response(
                    vec![sylvander_llm_core::ContentBlock::Text { text: "two".into() }],
                ))),
            ],
            vec![Ok(sylvander_llm_core::ModelStreamEvent::ReasoningDelta(
                "not allowed".into(),
            ))],
            vec![Ok(sylvander_llm_core::ModelStreamEvent::Completed(
                response(vec![sylvander_llm_core::ContentBlock::Text {
                    text: "  ".into(),
                }]),
            ))],
        ];
        let legacy_model = crate::provider_compat::model_metadata_from_core(&provider_model());
        for events in cases {
            let llm = ProviderAutoCompactLlm::new(provider(events), provider_model());
            assert!(matches!(
                llm.summarize(&[MessageParam::user("old")], &legacy_model).await,
                Err(AgentLoopError::Provider { source, .. })
                    if source.kind == sylvander_llm_core::ProviderErrorKind::Protocol
            ));
        }
    }
}
