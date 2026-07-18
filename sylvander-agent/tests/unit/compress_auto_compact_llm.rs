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
            Vec<Result<sylvander_llm_core::ModelStreamEvent, sylvander_llm_core::ProviderError>>,
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

fn response(content: Vec<sylvander_llm_core::ContentBlock>) -> sylvander_llm_core::ModelResponse {
    sylvander_llm_core::ModelResponse {
        id: "summary".into(),
        model: sylvander_llm_core::ModelRef::new("local", "model-a"),
        content,
        stop_reason: sylvander_llm_core::StopReason::EndTurn,
        usage: sylvander_llm_core::TokenUsage::default(),
    }
}

fn provider(
    events: Vec<Result<sylvander_llm_core::ModelStreamEvent, sylvander_llm_core::ProviderError>>,
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
    let wire_model = crate::provider_adapter::model_metadata_from_core(&provider_model());
    let summary = llm
        .summarize(&[MessageParam::user("old context")], &wire_model)
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
        request.tools.is_empty() && request.reasoning.is_none() && request.output_schema.is_none()
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
    let wire_model = crate::provider_adapter::model_metadata_from_core(&provider_model());
    for events in cases {
        let llm = ProviderAutoCompactLlm::new(provider(events), provider_model());
        assert!(matches!(
            llm.summarize(&[MessageParam::user("old")], &wire_model).await,
            Err(AgentLoopError::Provider { source, .. })
                if source.kind == sylvander_llm_core::ProviderErrorKind::Protocol
        ));
    }
}
