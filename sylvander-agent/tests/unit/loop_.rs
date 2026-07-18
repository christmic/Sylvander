use super::*;
use crate::test_support::MockTool;
use serde_json::json;
use sylvander_llm_anthropic::api::model::ModelCapabilities;
use sylvander_llm_core::{
    CacheHint, ChatMessage, ChatRole, ContentBlock as ProviderBlock, DocumentContent, ImageContent,
    MediaSource, ModelCapabilities as ProviderCapabilities, ModelEventStream, ModelRef,
    ModelResponse, ModelStreamEvent, ProviderError, ProviderErrorKind, ProviderErrorPhase,
    ProviderFuture, StopReason as ProviderStopReason, SystemInstruction, TokenUsage,
    ToolResultContent,
};

type ProviderOpen = Result<Vec<Result<ModelStreamEvent, ProviderError>>, ProviderError>;

struct ScriptedProvider {
    opens: std::sync::Mutex<std::collections::VecDeque<ProviderOpen>>,
    requests: std::sync::Mutex<Vec<ModelRequest>>,
}

impl ScriptedProvider {
    fn new(opens: impl IntoIterator<Item = ProviderOpen>) -> Self {
        Self {
            opens: std::sync::Mutex::new(opens.into_iter().collect()),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl ModelProvider for ScriptedProvider {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
        self.requests.lock().unwrap().push(request);
        let open = self.opens.lock().unwrap().pop_front().unwrap();
        Box::pin(async move {
            open.map(|events| Box::pin(futures_util::stream::iter(events)) as ModelEventStream)
        })
    }
}

struct FakeProvider {
    _secret: &'static str,
}

impl ModelProvider for FakeProvider {
    fn complete_stream(&self, _request: sylvander_llm_core::ModelRequest) -> ProviderFuture<'_> {
        Box::pin(async {
            let stream: ModelEventStream = Box::pin(futures_util::stream::empty());
            Ok(stream)
        })
    }
}

struct SlowTool;

#[async_trait::async_trait]
impl crate::tool::Tool for SlowTool {
    fn name(&self) -> &'static str {
        "slow"
    }

    fn description(&self) -> &'static str {
        "waits beyond its deadline"
    }

    fn input_schema(&self) -> sylvander_llm_anthropic::api::types::InputSchema {
        sylvander_llm_anthropic::api::types::InputSchema::empty()
    }

    async fn execute(
        &self,
        _ctx: &crate::tool_context::ToolContext,
        _input: serde_json::Value,
    ) -> Result<crate::tool::ToolOutput, crate::tool::ToolError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn tool_deadline_is_a_typed_outcome() {
    let tools = crate::tool::ToolRegistry::new().register(SlowTool);
    let gateway =
        crate::tool_invocation::RegistryBoundToolGateway::new(tools.invocation_descriptors());
    let snapshot = crate::tool_invocation::ToolInvocationGateway::snapshot(gateway.as_ref());
    let outcome = execute_registered_tool(RegisteredToolExecutionRequest {
        tool: tools.get("slow"),
        invocation_gateway: gateway,
        invocation_snapshot: snapshot,
        tool_context: crate::tool_context::defaults::system_tool_context(),
        input: serde_json::json!({}),
        call_id: "call-slow".into(),
        route: "slow".into(),
        timeout: Some(std::time::Duration::from_millis(1)),
        progress: crate::tool::ToolProgressSink::new(|_| {}),
    })
    .await;
    assert_eq!(
        outcome.timed_out_after,
        Some(std::time::Duration::from_millis(1))
    );
    assert!(outcome.is_error);
    assert!(outcome.output.contains("timed out"));
}

fn shadow_model(model_id: &str) -> ModelInfo {
    ModelInfo::builder()
        .id(model_id)
        .context_window(200_000)
        .max_output_tokens(8192)
        .capability(ModelCapabilities::TOOL_USE)
        .build()
        .expect("model build")
}

fn provider_model() -> ProviderModelInfo {
    provider_model_for("local", "test-model")
}

fn provider_model_for(provider_id: &str, model_id: &str) -> ProviderModelInfo {
    ProviderModelInfo {
        reference: ModelRef::new(provider_id, model_id),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: ProviderCapabilities::TOOL_USE,
    }
}

fn loop_builder() -> AgentLoopBuilder {
    AgentLoop::builder().tool_context(crate::tool_context::defaults::system_tool_context())
}

#[test]
fn builder_requires_qualified_router() {
    let result = loop_builder().provider_model(provider_model()).build();
    match result {
        Err(AgentLoopError::Builder(msg)) => assert!(msg.contains("qualified router")),
        other => panic!("expected Builder error, got {other:?}"),
    }
}

#[test]
fn builder_requires_model() {
    let result = loop_builder()
        .qualified_router(Arc::new(FakeProvider { _secret: "secret" }))
        .build();
    match result {
        Err(AgentLoopError::Builder(msg)) => assert!(msg.contains("provider model")),
        other => panic!("expected Builder error, got {other:?}"),
    }
}

#[test]
fn builder_succeeds_with_required_fields() {
    let loop_ = loop_builder()
        .qualified_router(Arc::new(FakeProvider { _secret: "secret" }))
        .provider_model(provider_model())
        .build()
        .expect("build should succeed");
    assert_eq!(loop_.model().id.as_str(), "test-model");
    assert_eq!(loop_.max_iterations(), 50);
    assert_eq!(loop_.max_retries(), 3);
}

#[test]
fn builder_requires_an_explicit_tool_context() {
    let result = AgentLoop::builder()
        .qualified_router(Arc::new(FakeProvider { _secret: "secret" }))
        .provider_model(provider_model())
        .build();
    assert!(
        matches!(result, Err(AgentLoopError::Builder(message)) if message.contains("tool context"))
    );
}

#[tokio::test]
async fn inert_agent_run_template_stops_before_model_or_tool_dispatch() {
    let provider = Arc::new(ScriptedProvider::new(Vec::<ProviderOpen>::new()));
    let loop_ = AgentLoop::builder()
        .qualified_router(provider.clone())
        .provider_model(provider_model())
        .tool_context(crate::tool_context::ToolContext::inert_agent_run_template())
        .tool(MockTool::new(
            "must_not_run",
            "would prove an invalid construction context escaped",
            crate::tool::ToolOutput::ok("unexpected"),
        ))
        .build()
        .unwrap();

    let events = run_stream(&loop_, vec![MessageParam::user("go")])
        .collect::<Vec<_>>()
        .await;

    assert!(
        matches!(
            events.as_slice(),
            [AgentEvent::Error(AgentLoopError::Tool(message))]
                if message.contains("not initialized")
        ),
        "{events:?}"
    );
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[test]
fn provider_builder_preserves_qualified_identity_and_safe_debug() {
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider {
        _secret: "secret-provider-state",
    });
    let builder = loop_builder()
        .qualified_router(provider)
        .provider_model(provider_model());
    let debug = format!("{builder:?}");
    assert!(!debug.contains("secret-provider-state"));
    let loop_ = builder.build().unwrap();
    assert_eq!(loop_.model.id, "test-model");
    assert_eq!(
        loop_.provider_model.reference,
        ModelRef::new("local", "test-model")
    );
}

#[test]
fn prompt_cache_hints_follow_the_selected_model_capability() {
    for enabled in [false, true] {
        let capabilities = if enabled {
            ProviderCapabilities::TOOL_USE | ProviderCapabilities::PROMPT_CACHING
        } else {
            ProviderCapabilities::TOOL_USE
        };
        let model = ProviderModelInfo {
            reference: ModelRef::new("local", "cache-model"),
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities,
        };
        let loop_ = loop_builder()
            .qualified_router(Arc::new(FakeProvider {
                _secret: "not-resolved",
            }))
            .provider_model(model)
            .system_prompt("stable instructions")
            .tool(MockTool::new(
                "read",
                "read a file",
                crate::tool::ToolOutput::ok("done"),
            ))
            .build()
            .unwrap();

        let neutral = loop_
            .build_provider_request(&[MessageParam::user("go")])
            .unwrap();
        assert_eq!(neutral.system[0].cache_hint.is_some(), enabled);
        assert_eq!(neutral.tools[0].cache_hint.is_some(), enabled);
    }
}

#[test]
fn lossy_message_cache_metadata_fails_before_dispatch() {
    use sylvander_llm_anthropic::api::types::{CacheControl, TextBlock, UserContentBlock};

    let provider = Arc::new(ScriptedProvider::new(Vec::<ProviderOpen>::new()));
    let loop_ = loop_builder()
        .qualified_router(provider.clone())
        .provider_model(provider_model())
        .max_retries(0)
        .build()
        .unwrap();
    let messages = [MessageParam::user_blocks(vec![UserContentBlock::Text(
        TextBlock::new("secret-text").with_cache_control(CacheControl::ephemeral()),
    )])];
    let error = loop_
        .build_provider_request(&messages)
        .expect_err("lossy cache metadata must fail before provider dispatch");
    assert!(matches!(error, AgentLoopError::Validation(_)));
    assert!(!error.to_string().contains("secret-text"));
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[test]
fn qualified_router_accepts_cross_provider_runtime_model() {
    let router: Arc<dyn ModelProvider> = Arc::new(FakeProvider { _secret: "secret" });
    let mut loop_ = loop_builder()
        .qualified_router(router)
        .provider_model(provider_model())
        .build()
        .unwrap();
    let selection = ModelSelection {
        provider_id: "remote".into(),
        model_id: "model-b".into(),
    };
    loop_
        .apply_runtime_model(
            &selection,
            &shadow_model("model-b"),
            Some(&provider_model_for("remote", "model-b")),
        )
        .unwrap();
    assert_eq!(loop_.model.id, "model-b");
    assert_eq!(
        loop_.provider_model.reference,
        ModelRef::new("remote", "model-b")
    );
}

#[test]
fn qualified_router_rejects_any_runtime_identity_mismatch() {
    let router: Arc<dyn ModelProvider> = Arc::new(FakeProvider { _secret: "secret" });
    let mut loop_ = loop_builder()
        .qualified_router(router)
        .provider_model(provider_model())
        .build()
        .unwrap();
    let selection = ModelSelection {
        provider_id: "remote".into(),
        model_id: "model-b".into(),
    };
    let cases = [
        (
            shadow_model("model-b"),
            provider_model_for("remote", "wrong"),
        ),
        (
            shadow_model("wrong"),
            provider_model_for("remote", "model-b"),
        ),
        (
            shadow_model("model-b"),
            provider_model_for("wrong", "model-b"),
        ),
    ];
    for (shadow, exact) in cases {
        assert!(matches!(
            loop_.apply_runtime_model(&selection, &shadow, Some(&exact)),
            Err(AgentLoopError::IncompatibleModel(_))
        ));
    }
    assert_eq!(
        loop_.provider_model.reference,
        ModelRef::new("local", "test-model")
    );
}

fn completed_events(
    content: Vec<ProviderBlock>,
    stop_reason: ProviderStopReason,
) -> Vec<Result<ModelStreamEvent, ProviderError>> {
    vec![Ok(ModelStreamEvent::Completed(ModelResponse {
        id: "response".into(),
        model: ModelRef::new("local", "test-model"),
        content,
        stop_reason,
        usage: TokenUsage::default(),
    }))]
}

fn neutral_request() -> ModelRequest {
    ModelRequest {
        request_id: "secret-request".into(),
        model: ModelRef::new("local", "test-model"),
        system: Vec::new(),
        messages: vec![ChatMessage::user("hello")],
        tools: Vec::new(),
        max_output_tokens: 100,
        reasoning: None,
        output_schema: None,
    }
}

fn neutral_image() -> ImageContent {
    ImageContent {
        source: MediaSource::Url {
            url: "https://secret.invalid/image".into(),
        },
        alt_text: None,
    }
}

fn neutral_document() -> DocumentContent {
    DocumentContent {
        source: MediaSource::Url {
            url: "https://secret.invalid/document".into(),
        },
        title: Some("secret-document".into()),
    }
}

fn provider_loop_with_capabilities(
    provider: Arc<ScriptedProvider>,
    capabilities: ProviderCapabilities,
) -> AgentLoop {
    loop_builder()
        .qualified_router(provider)
        .provider_model(ProviderModelInfo {
            reference: ModelRef::new("local", "test-model"),
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities,
        })
        .build()
        .unwrap()
}

#[tokio::test]
async fn provider_capability_preflight_rejects_before_dispatch() {
    let mut tool_call = neutral_request();
    tool_call.messages.push(ChatMessage {
        role: ChatRole::Assistant,
        content: vec![ProviderBlock::ToolCall {
            id: "secret-call".into(),
            name: "secret-tool".into(),
            arguments: json!({"secret": true}),
        }],
    });
    let mut tool_result = neutral_request();
    tool_result.messages.push(ChatMessage {
        role: ChatRole::User,
        content: vec![ProviderBlock::ToolResult {
            call_id: "secret-call".into(),
            content: vec![ToolResultContent::Text {
                text: "secret-result".into(),
            }],
            is_error: false,
        }],
    });
    let mut reasoning = neutral_request();
    reasoning.messages.push(ChatMessage {
        role: ChatRole::Assistant,
        content: vec![ProviderBlock::Reasoning {
            text: "secret-reasoning".into(),
            opaque_state: None,
        }],
    });
    let mut image = neutral_request();
    image.messages.push(ChatMessage {
        role: ChatRole::User,
        content: vec![ProviderBlock::Image {
            image: neutral_image(),
        }],
    });
    let mut document = neutral_request();
    document.messages.push(ChatMessage {
        role: ChatRole::User,
        content: vec![ProviderBlock::Document {
            document: neutral_document(),
        }],
    });
    let mut schema = neutral_request();
    schema.output_schema = Some(json!({"secret-schema": true}));
    let mut cache = neutral_request();
    cache.system.push(SystemInstruction {
        text: "secret-system".into(),
        cache_hint: Some(CacheHint::Ephemeral),
    });

    let provider = Arc::new(ScriptedProvider::new(Vec::<ProviderOpen>::new()));
    let loop_ = provider_loop_with_capabilities(provider.clone(), ProviderCapabilities::empty());
    for request in [
        tool_call,
        tool_result,
        reasoning,
        image,
        document,
        schema,
        cache,
    ] {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let Err(error) = loop_.call_model_with_retry(request, tx).await else {
            panic!("unsupported request reached provider dispatch");
        };
        assert!(matches!(error, AgentLoopError::IncompatibleModel(_)));
        assert!(!error.is_retryable());
        assert!(!error.to_string().contains("secret"));
    }
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn provider_capability_preflight_dispatches_once_when_fully_supported() {
    let provider = Arc::new(ScriptedProvider::new([Ok(completed_events(
        vec![ProviderBlock::Text { text: "ok".into() }],
        ProviderStopReason::EndTurn,
    ))]));
    let all = ProviderCapabilities::TOOL_USE
        | ProviderCapabilities::REASONING
        | ProviderCapabilities::STRUCTURED_OUTPUT
        | ProviderCapabilities::PROMPT_CACHING
        | ProviderCapabilities::VISION
        | ProviderCapabilities::DOCUMENT_INPUT;
    let loop_ = provider_loop_with_capabilities(provider.clone(), all);
    let mut request = neutral_request();
    request.output_schema = Some(json!({"type": "object"}));
    request.system.push(SystemInstruction {
        text: "system".into(),
        cache_hint: Some(CacheHint::Ephemeral),
    });
    request.reasoning = Some(sylvander_llm_core::ReasoningConfig { budget_tokens: 10 });
    request.messages.push(ChatMessage {
        role: ChatRole::Assistant,
        content: vec![
            ProviderBlock::Reasoning {
                text: "reasoning".into(),
                opaque_state: None,
            },
            ProviderBlock::ToolCall {
                id: "call".into(),
                name: "tool".into(),
                arguments: json!({}),
            },
        ],
    });
    request.messages.push(ChatMessage {
        role: ChatRole::User,
        content: vec![ProviderBlock::ToolResult {
            call_id: "call".into(),
            content: vec![
                ToolResultContent::Image {
                    image: neutral_image(),
                },
                ToolResultContent::Document {
                    document: neutral_document(),
                },
            ],
            is_error: false,
        }],
    });
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    loop_.call_model_with_retry(request, tx).await.unwrap();
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn provider_backend_runs_tool_then_text_with_qualified_requests() {
    let provider = Arc::new(ScriptedProvider::new([
        Ok(completed_events(
            vec![ProviderBlock::ToolCall {
                id: "call-1".into(),
                name: "echo".into(),
                arguments: json!({"value": 7}),
            }],
            ProviderStopReason::ToolUse,
        )),
        Ok(completed_events(
            vec![ProviderBlock::Text {
                text: "done".into(),
            }],
            ProviderStopReason::EndTurn,
        )),
    ]));
    let tool = MockTool::new("echo", "echo input", crate::tool::ToolOutput::ok("7"));
    let loop_ = loop_builder()
        .qualified_router(provider.clone())
        .provider_model(provider_model())
        .tool(tool.clone())
        .build()
        .unwrap();
    let result = run(&loop_, vec![MessageParam::user("start")])
        .await
        .unwrap();
    assert_eq!(result.iterations, 2);
    assert_eq!(tool.call_count(), 1);
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.model == ModelRef::new("local", "test-model"))
    );
    assert!(requests[1].messages.iter().any(|message| {
        message.content.iter().any(|block|
            matches!(block, ProviderBlock::ToolResult { call_id, .. } if call_id == "call-1")
        )
    }));
}

#[tokio::test]
async fn provider_open_retry_and_stream_protocol_are_typed() {
    let unavailable = ProviderError::new(
        ProviderErrorKind::Unavailable,
        ProviderErrorPhase::Open,
        "temporarily unavailable",
    );
    let provider = Arc::new(ScriptedProvider::new([
        Err(unavailable),
        Ok(completed_events(
            vec![ProviderBlock::Text { text: "ok".into() }],
            ProviderStopReason::EndTurn,
        )),
    ]));
    let loop_ = loop_builder()
        .qualified_router(provider.clone())
        .provider_model(provider_model())
        .max_retries(1)
        .build()
        .unwrap();
    assert!(run(&loop_, vec![MessageParam::user("retry")]).await.is_ok());
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].request_id, requests[1].request_id);
    }

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let empty: ModelEventStream = Box::pin(futures_util::stream::empty());
    let error = consume_provider_stream(empty, ModelRef::new("local", "test-model"), &tx)
        .await
        .unwrap_err();
    assert!(
        matches!(error, AgentLoopError::Provider { source, .. } if source.kind == ProviderErrorKind::Protocol)
    );

    let events = completed_events(Vec::new(), ProviderStopReason::EndTurn)
        .into_iter()
        .chain([Ok(ModelStreamEvent::TextDelta("late".into()))]);
    let stream: ModelEventStream = Box::pin(futures_util::stream::iter(events));
    let error = consume_provider_stream(stream, ModelRef::new("local", "test-model"), &tx)
        .await
        .unwrap_err();
    assert!(
        matches!(error, AgentLoopError::Provider { source, .. } if source.kind == ProviderErrorKind::Protocol)
    );
}

#[test]
fn qualified_builder_rejects_an_incomplete_route() {
    let provider = || Arc::new(FakeProvider { _secret: "secret" }) as Arc<dyn ModelProvider>;
    assert!(matches!(
        loop_builder().qualified_router(provider()).build(),
        Err(AgentLoopError::Builder(message)) if message.contains("provider model")
    ));
    assert!(matches!(
        loop_builder().provider_model(provider_model()).build(),
        Err(AgentLoopError::Builder(message)) if message.contains("qualified router")
    ));
}

#[test]
fn builder_sets_max_iterations() {
    let loop_ = loop_builder()
        .qualified_router(Arc::new(FakeProvider { _secret: "secret" }))
        .provider_model(provider_model())
        .max_iterations(10)
        .build()
        .expect("build");
    assert_eq!(loop_.max_iterations(), 10);
}

#[test]
fn builder_sets_max_retries() {
    let loop_ = loop_builder()
        .qualified_router(Arc::new(FakeProvider { _secret: "secret" }))
        .provider_model(provider_model())
        .max_retries(0)
        .build()
        .expect("build");
    assert_eq!(loop_.max_retries(), 0);
}

#[test]
fn reasoning_effort_builds_a_capability_checked_budget() {
    let model = ProviderModelInfo {
        reference: ModelRef::new("local", "thinking-model"),
        context_window: 200_000,
        max_output_tokens: 8_192,
        capabilities: ProviderCapabilities::REASONING,
    };
    let loop_ = loop_builder()
        .qualified_router(Arc::new(FakeProvider { _secret: "secret" }))
        .provider_model(model)
        .reasoning_effort(sylvander_protocol::ReasoningEffort::High)
        .build()
        .expect("loop");
    let request = loop_
        .build_provider_request(&[MessageParam::user("think")])
        .expect("provider request");
    assert_eq!(request.reasoning.unwrap().budget_tokens, 8_192);
    assert_eq!(
        loop_.reasoning_effort(),
        sylvander_protocol::ReasoningEffort::High
    );
}

#[test]
fn retry_cause_distinguishes_rate_limit_server_and_stream_failures() {
    let provider_error = |kind, phase| ProviderError::new(kind, phase, "failed");
    assert_eq!(
        provider_retry_cause(&provider_error(
            ProviderErrorKind::RateLimited,
            ProviderErrorPhase::Open,
        )),
        sylvander_protocol::RetryCause::RateLimit
    );
    assert_eq!(
        provider_retry_cause(&provider_error(
            ProviderErrorKind::Unavailable,
            ProviderErrorPhase::Open,
        )),
        sylvander_protocol::RetryCause::Server
    );
    assert_eq!(
        provider_retry_cause(&provider_error(
            ProviderErrorKind::Protocol,
            ProviderErrorPhase::Stream,
        )),
        sylvander_protocol::RetryCause::Stream
    );
}

#[test]
fn builder_registers_tool() {
    let tool = MockTool::new("echo", "echoes", super::super::tool::ToolOutput::ok("hi"));
    let loop_ = loop_builder()
        .qualified_router(Arc::new(FakeProvider { _secret: "secret" }))
        .provider_model(provider_model())
        .tool(tool)
        .build()
        .expect("build");
    assert_eq!(loop_.tools().len(), 1);
    assert!(loop_.tools().get("echo").is_some());
}

#[test]
fn default_max_iterations_is_50() {
    let loop_ = loop_builder()
        .qualified_router(Arc::new(FakeProvider { _secret: "secret" }))
        .provider_model(provider_model())
        .build()
        .expect("build");
    assert_eq!(loop_.max_iterations(), 50);
}

#[test]
fn cumulative_usage_saturates_and_preserves_optional_cache_semantics() {
    let total = Usage {
        input_tokens: u32::MAX - 1,
        output_tokens: 10,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: Some(u32::MAX),
    };
    let next = Usage {
        input_tokens: 10,
        output_tokens: u32::MAX,
        cache_creation_input_tokens: Some(4),
        cache_read_input_tokens: None,
    };

    let cumulative = saturating_add_usage(&total, &next);
    assert_eq!(cumulative.input_tokens, u32::MAX);
    assert_eq!(cumulative.output_tokens, u32::MAX);
    assert_eq!(cumulative.cache_creation_input_tokens, Some(4));
    assert_eq!(cumulative.cache_read_input_tokens, Some(u32::MAX));
    assert_eq!(saturating_add_optional_tokens(None, None), None);
}

#[test]
fn agent_run_debug_impl() {
    let run = AgentLoopResult {
        final_message: Message {
            id: "msg_x".into(),
            kind: sylvander_llm_anthropic::api::types::MessageKind::Message,
            role: sylvander_llm_anthropic::api::types::MessageRole::Assistant,
            content: vec![],
            model: "test-model".into(),
            stop_reason: Some(sylvander_llm_anthropic::api::types::StopReason::EndTurn),
            stop_sequence: None,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        },
        iterations: 1,
        total_usage: Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
    };
    let _ = format!("{run:?}");
    let _ = json!({});
}
