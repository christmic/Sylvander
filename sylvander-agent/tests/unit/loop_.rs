use super::*;
use crate::approval::{ApprovalBatchResult, ApprovalDecision, ApprovalGate, ToolUseRequest};
use crate::test_support::MockTool;
use crate::tool_invocation::ToolInvocationGateway as _;
use crate::turn::conversation::ConversationSnapshot;
use serde_json::json;
use sylvander_llm_core::{
    CacheHint, ChatMessage, ChatRole, ContentBlock as ProviderBlock, DocumentContent, ImageContent,
    InputSchema, MediaSource, ModelCapabilities as ProviderCapabilities, ModelEventStream,
    ModelInfo as ProviderModelInfo, ModelRef, ModelResponse, ModelStreamEvent, ProviderError,
    ProviderErrorKind, ProviderErrorPhase, ProviderFuture,
    ReasoningEffort as ProviderReasoningEffort, StopReason as ProviderStopReason,
    SystemInstruction, TokenUsage, ToolResultContent,
};

type ProviderOpen = Result<Vec<Result<ModelStreamEvent, ProviderError>>, ProviderError>;

struct ScriptedProvider {
    opens: std::sync::Mutex<std::collections::VecDeque<ProviderOpen>>,
    requests: std::sync::Mutex<Vec<ModelRequest>>,
}

#[derive(Default)]
struct RecordingApprovalGate {
    requests: std::sync::Mutex<Vec<ToolUseRequest>>,
}

#[async_trait::async_trait]
impl ApprovalGate for RecordingApprovalGate {
    async fn check_batch(&self, tools: &[ToolUseRequest]) -> ApprovalBatchResult {
        self.requests.lock().unwrap().extend_from_slice(tools);
        ApprovalBatchResult {
            decisions: vec![ApprovalDecision::Approved; tools.len()],
        }
    }
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

struct SlowTool;

impl crate::tool::ToolDefinition for SlowTool {
    fn spec(&self) -> crate::tool::ToolSpec {
        crate::tool::ToolSpec::immediate(
            "slow",
            "waits beyond its deadline",
            InputSchema::empty().schema,
            crate::tool_invocation::ToolInvocationClass::Extension,
        )
    }
}

#[async_trait::async_trait]
impl crate::tool::ToolExecutor for SlowTool {
    async fn handle(
        &self,
        _ctx: &crate::tool_context::ToolContext,
        _call: &crate::tool::PreparedToolCall,
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
        prepared_call: tools.prepare("slow", serde_json::json!({})),
        invocation_gateway: gateway,
        invocation_snapshot: snapshot,
        tool_context: crate::tool_context::defaults::system_tool_context(),
        call_id: "call-slow".into(),
        invocation_id: "00000000-0000-4000-8000-000000000005".into(),
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

#[tokio::test]
async fn recovery_reenters_the_authorized_tool_boundary_with_a_stable_identity() {
    let tools = crate::tool::ToolRegistry::new().register(MockTool::new(
        "recoverable",
        "recovery test",
        crate::tool::ToolOutput::ok("recovered"),
    ));
    let gateway =
        crate::tool_invocation::RegistryBoundToolGateway::new(tools.invocation_descriptors());
    let snapshot = crate::tool_invocation::ToolInvocationGateway::snapshot(gateway.as_ref());
    let output = execute_recovery_tool(RecoveryToolRequest {
        tools,
        invocation_gateway: gateway,
        invocation_snapshot: snapshot,
        tool_context: crate::tool_context::defaults::system_tool_context(),
        call_id: "call-recovery".into(),
        invocation_id: "00000000-0000-4000-8000-000000000006".into(),
        route: "recoverable".into(),
        input: serde_json::json!({}),
    })
    .await;

    assert_eq!(output.output, "recovered");
    assert!(!output.is_error);
}

#[tokio::test]
async fn timed_out_tool_emits_one_authoritative_terminal() {
    let provider = Arc::new(ScriptedProvider::new([
        Ok(completed_events(
            vec![ProviderBlock::ToolCall {
                id: "call-slow".into(),
                name: "slow".into(),
                arguments: json!({}),
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
    let tools = crate::tool::ToolRegistry::new().register(SlowTool);
    let request = turn_request(provider_model(), tools, vec![ChatMessage::user("start")]);
    let mut ports = turn_ports(provider, &request);
    ports.tool_context.budget.timeout = Some(std::time::Duration::from_millis(1));
    let kernel = kernel();
    let mut events = Box::pin(run_stream(&kernel, request, ports));
    let mut timeout_events = 0;
    let mut terminal_events = 0;

    while let Some(event) = events.next().await {
        match event {
            AgentEvent::ToolTimedOut { id, .. } if id == "call-slow" => timeout_events += 1,
            AgentEvent::ToolCallEnd {
                id,
                is_error: true,
                failure_kind: Some(crate::tool::ToolFailureKind::Unclassified),
                ..
            } if id == "call-slow" => terminal_events += 1,
            _ => {}
        }
    }

    assert_eq!(timeout_events, 1);
    assert_eq!(terminal_events, 1);
}

#[tokio::test]
async fn turn_transitions_describe_the_real_multi_iteration_path() {
    let provider = Arc::new(ScriptedProvider::new([
        Ok(completed_events(
            vec![ProviderBlock::ToolCall {
                id: "call-1".into(),
                name: "echo".into(),
                arguments: json!({}),
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
    let tools = crate::tool::ToolRegistry::new().register(MockTool::new(
        "echo",
        "echoes",
        crate::tool::ToolOutput::ok("ok"),
    ));
    let request = turn_request(provider_model(), tools, vec![ChatMessage::user("start")]);
    let ports = turn_ports(provider, &request);
    let kernel = kernel();
    let mut events = Box::pin(run_stream(&kernel, request, ports));
    let mut transitions = Vec::new();

    while let Some(event) = events.next().await {
        if let AgentEvent::TurnTransition(transition) = event {
            transitions.push(transition);
        }
    }

    assert!(
        transitions
            .windows(2)
            .all(|pair| pair[1].sequence == pair[0].sequence + 1)
    );
    assert_eq!(transitions.last().unwrap().to, TurnPhase::Completed);
    let continued = transitions
        .iter()
        .find(|transition| transition.reason == TurnTransitionReason::ContinueAfterToolResults)
        .unwrap();
    assert_eq!(continued.to, TurnPhase::ReadyForIteration);
    assert_eq!(
        continued.continuation,
        Some(TurnContinuationReason::ToolResultsReady)
    );
    assert_eq!(continued.iteration, 1);
    assert_eq!(
        crate::turn::machine::TurnSnapshot::from(*continued).phase,
        continued.to
    );
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

fn kernel() -> AgentLoop {
    AgentLoop::builder().build()
}

fn turn_request(
    model: ProviderModelInfo,
    tools: crate::tool::ToolRegistry,
    messages: Vec<ChatMessage>,
) -> AgentTurnRequest {
    AgentTurnRequest {
        conversation: ConversationSnapshot::new(messages),
        model,
        system_instructions: Vec::new(),
        reasoning: None,
        tools,
        execution: crate::execution_context::AgentExecutionContext::restricted_for(
            "user", "agent", "session",
        ),
    }
}

fn turn_ports(provider: Arc<dyn ModelProvider>, request: &AgentTurnRequest) -> AgentExecutionPorts {
    let gateway = crate::tool_invocation::RegistryBoundToolGateway::new(
        request.tools.invocation_descriptors(),
    );
    let snapshot = gateway.snapshot();
    AgentExecutionPorts::new(
        provider,
        crate::tool_context::ToolContext::new(request.execution.clone()),
        gateway,
        snapshot,
    )
}

#[test]
fn kernel_builder_contains_only_stable_loop_policy() {
    let loop_ = kernel();
    assert_eq!(loop_.max_iterations(), 50);
    assert_eq!(loop_.max_retries(), 3);
    let debug = format!("{loop_:?}");
    assert!(!debug.contains("model"));
    assert!(!debug.contains("tool_context"));
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
        let tools = crate::tool::ToolRegistry::new().register(MockTool::new(
            "read",
            "read a file",
            crate::tool::ToolOutput::ok("done"),
        ));
        let mut request = turn_request(model, tools, vec![ChatMessage::user("go")]);
        request.system_instructions = vec![SystemInstruction {
            text: "stable instructions".into(),
            cache_hint: enabled.then_some(CacheHint::Ephemeral),
        }];
        let neutral = AgentLoop::build_provider_request(&request, request.conversation.messages());
        assert_eq!(neutral.system[0].cache_hint.is_some(), enabled);
        assert_eq!(neutral.tools[0].cache_hint.is_some(), enabled);
    }
}

#[test]
fn provider_neutral_message_builds_without_protocol_translation() {
    let provider = Arc::new(ScriptedProvider::new(Vec::<ProviderOpen>::new()));
    let messages = [ChatMessage::user("neutral-text")];
    let turn = turn_request(
        provider_model(),
        crate::tool::ToolRegistry::new(),
        messages.to_vec(),
    );
    let request = AgentLoop::build_provider_request(&turn, turn.conversation.messages());
    assert_eq!(request.messages, messages);
    assert!(provider.requests.lock().unwrap().is_empty());
}

fn completed_events(
    content: Vec<ProviderBlock>,
    stop_reason: ProviderStopReason,
) -> Vec<Result<ModelStreamEvent, ProviderError>> {
    vec![Ok(ModelStreamEvent::Completed(Box::new(ModelResponse {
        id: "response".into(),
        model: ModelRef::new("local", "test-model"),
        content,
        stop_reason,
        usage: TokenUsage::default(),
    })))]
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
    _provider: Arc<ScriptedProvider>,
    capabilities: ProviderCapabilities,
) -> (AgentLoop, ProviderModelInfo) {
    (
        kernel(),
        ProviderModelInfo {
            reference: ModelRef::new("local", "test-model"),
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities,
        },
    )
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
    let (loop_, model) =
        provider_loop_with_capabilities(provider.clone(), ProviderCapabilities::empty());
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
        let Err(error) = loop_
            .call_model_with_retry(request, tx, provider.as_ref(), &model)
            .await
        else {
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
    let (loop_, model) = provider_loop_with_capabilities(provider.clone(), all);
    let mut request = neutral_request();
    request.output_schema = Some(json!({"type": "object"}));
    request.system.push(SystemInstruction {
        text: "system".into(),
        cache_hint: Some(CacheHint::Ephemeral),
    });
    request.reasoning = Some(sylvander_llm_core::ReasoningConfig {
        budget_tokens: Some(10),
        effort: None,
    });
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
    loop_
        .call_model_with_retry(request, tx, provider.as_ref(), &model)
        .await
        .unwrap();
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
    let tools = crate::tool::ToolRegistry::new().register(tool.clone());
    let request = turn_request(provider_model(), tools, vec![ChatMessage::user("start")]);
    let ports = turn_ports(provider.clone(), &request);
    let result = run(&kernel(), request, ports).await.unwrap();
    assert_eq!(result.iterations, 2);
    assert_eq!(result.conversation.messages().len(), 4);
    assert_eq!(result.final_response.text(), "done");
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
async fn trusted_tool_failure_classification_reaches_the_event_stream() {
    let provider = Arc::new(ScriptedProvider::new([
        Ok(completed_events(
            vec![ProviderBlock::ToolCall {
                id: "call-policy".into(),
                name: "guarded".into(),
                arguments: json!({}),
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
    let tool = MockTool::new(
        "guarded",
        "returns an explicit policy denial",
        crate::tool::ToolOutput::classified_err(
            "workspace boundary denied",
            crate::tool::ToolFailureKind::FilesystemBoundaryPolicyViolation,
        ),
    );
    let tools = crate::tool::ToolRegistry::new().register(tool);
    let request = turn_request(provider_model(), tools, vec![ChatMessage::user("start")]);
    let ports = turn_ports(provider, &request);
    let kernel = kernel();
    let mut events = Box::pin(run_stream(&kernel, request, ports));
    let mut classified = false;

    while let Some(event) = events.next().await {
        if matches!(
            event,
            AgentEvent::ToolCallEnd {
                id,
                name,
                failure_kind: Some(
                    crate::tool::ToolFailureKind::FilesystemBoundaryPolicyViolation
                ),
                ..
            } if id == "call-policy" && name == "guarded"
        ) {
            classified = true;
        }
    }

    assert!(classified);
}

#[tokio::test]
async fn tool_identity_is_prepared_before_approval_and_execution() {
    let provider = Arc::new(ScriptedProvider::new([
        Ok(completed_events(
            vec![ProviderBlock::ToolCall {
                id: "call-order".into(),
                name: "guarded".into(),
                arguments: json!({}),
            }],
            ProviderStopReason::ToolUse,
        )),
        Ok(completed_events(Vec::new(), ProviderStopReason::EndTurn)),
    ]));
    let tools = crate::tool::ToolRegistry::new().register(MockTool::new(
        "guarded",
        "ordered tool",
        crate::tool::ToolOutput::ok("done"),
    ));
    let request = turn_request(provider_model(), tools, vec![ChatMessage::user("start")]);
    let ports = turn_ports(provider, &request);
    let kernel = kernel();
    let events = run_stream(&kernel, request, ports)
        .collect::<Vec<_>>()
        .await;
    let prepared = events
        .iter()
        .position(
            |event| matches!(event, AgentEvent::ToolCallPrepared { id, .. } if id == "call-order"),
        )
        .unwrap();
    let AgentEvent::ToolCallPrepared {
        invocation_id,
        invocation_class,
        recovery_policy,
        input_digest,
        capability_revision,
        ..
    } = &events[prepared]
    else {
        unreachable!();
    };
    assert!(uuid::Uuid::parse_str(invocation_id).is_ok());
    assert_eq!(
        *invocation_class,
        Some(crate::tool_invocation::ToolInvocationClass::Extension),
    );
    assert_eq!(
        *recovery_policy,
        crate::tool_invocation::ToolRecoveryPolicy::NeverReplay,
    );
    assert!(input_digest.starts_with("sha256:"));
    assert!(capability_revision.starts_with("sha256:"));
    let started = events
        .iter()
        .position(
            |event| matches!(event, AgentEvent::ToolCallStart { id, .. } if id == "call-order"),
        )
        .unwrap();

    assert!(prepared < started);
}

#[tokio::test]
async fn approval_receives_facts_from_the_exact_prepared_call() {
    let provider = Arc::new(ScriptedProvider::new([
        Ok(completed_events(
            vec![ProviderBlock::ToolCall {
                id: "git-1".into(),
                name: "Git".into(),
                arguments: json!({"operation": "status"}),
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
    let gate = Arc::new(RecordingApprovalGate::default());
    let tools = crate::tool::ToolRegistry::new().register(crate::tools::GitTool::new());
    let request = turn_request(provider_model(), tools, vec![ChatMessage::user("inspect")]);
    let ports = turn_ports(provider, &request).with_approval_gate(gate.clone());

    run(&kernel(), request, ports).await.unwrap();

    let requests = gate.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].tool_name, "Git");
    assert_eq!(
        requests[0].facts.execution_mode,
        crate::tool::ToolExecutionMode::Parallel
    );
    assert_eq!(
        requests[0].facts.execution_policy,
        crate::tool::ToolExecutionPolicy::read_only_process()
    );
}

#[tokio::test]
async fn invalid_tool_input_is_rejected_before_approval() {
    let provider = Arc::new(ScriptedProvider::new([
        Ok(completed_events(
            vec![ProviderBlock::ToolCall {
                id: "command-1".into(),
                name: "Command".into(),
                arguments: json!({"command": "  "}),
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
    let gate = Arc::new(RecordingApprovalGate::default());
    let tools = crate::tool::ToolRegistry::new().register(crate::tools::CommandTool::new());
    let request = turn_request(provider_model(), tools, vec![ChatMessage::user("run")]);
    let ports = turn_ports(provider, &request).with_approval_gate(gate.clone());

    let outcome = run(&kernel(), request, ports).await.unwrap();

    assert!(gate.requests.lock().unwrap().is_empty());
    assert_eq!(outcome.iterations, 2);
    assert!(outcome.conversation.messages().iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(
                block,
                ProviderBlock::ToolResult {
                    call_id,
                    is_error: true,
                    ..
                } if call_id == "command-1"
            )
        })
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
    let loop_ = AgentLoop::builder().max_retries(1).build();
    let request = turn_request(
        provider_model(),
        crate::tool::ToolRegistry::new(),
        vec![ChatMessage::user("retry")],
    );
    let ports = turn_ports(provider.clone(), &request);
    assert!(run(&loop_, request, ports).await.is_ok());
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
fn builder_sets_max_iterations() {
    let loop_ = AgentLoop::builder().max_iterations(10).build();
    assert_eq!(loop_.max_iterations(), 10);
}

#[test]
fn builder_sets_max_retries() {
    let loop_ = AgentLoop::builder().max_retries(0).build();
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
    let mut turn = turn_request(
        model,
        crate::tool::ToolRegistry::new(),
        vec![ChatMessage::user("think")],
    );
    turn.reasoning = Some(sylvander_llm_core::ReasoningConfig {
        budget_tokens: Some(16_384),
        effort: Some(ProviderReasoningEffort::High),
    });
    let request = AgentLoop::build_provider_request(&turn, turn.conversation.messages());
    let reasoning = request.reasoning.expect("reasoning config");
    assert_eq!(reasoning.budget_tokens, Some(8_192));
    assert_eq!(reasoning.effort, Some(ProviderReasoningEffort::High));
}

#[test]
fn retry_cause_distinguishes_rate_limit_server_and_stream_failures() {
    let provider_error = |kind, phase| ProviderError::new(kind, phase, "failed");
    assert_eq!(
        provider_retry_cause(&provider_error(
            ProviderErrorKind::RateLimited,
            ProviderErrorPhase::Open,
        )),
        ModelRetryCause::RateLimit
    );
    assert_eq!(
        provider_retry_cause(&provider_error(
            ProviderErrorKind::Unavailable,
            ProviderErrorPhase::Open,
        )),
        ModelRetryCause::Server
    );
    assert_eq!(
        provider_retry_cause(&provider_error(
            ProviderErrorKind::Protocol,
            ProviderErrorPhase::Stream,
        )),
        ModelRetryCause::Stream
    );
}

#[test]
fn turn_request_owns_the_executable_tool_snapshot() {
    let tool = MockTool::new("echo", "echoes", crate::tool::ToolOutput::ok("hi"));
    let request = turn_request(
        provider_model(),
        crate::tool::ToolRegistry::new().register(tool),
        Vec::new(),
    );
    assert_eq!(request.tools.len(), 1);
    assert!(request.tools.get("echo").is_some());
}

#[test]
fn default_max_iterations_is_50() {
    let loop_ = AgentLoop::builder().build();
    assert_eq!(loop_.max_iterations(), 50);
}

#[test]
fn cumulative_usage_saturates_and_preserves_optional_cache_semantics() {
    let mut total = TokenUsage {
        input_tokens: u64::MAX - 1,
        output_tokens: 10,
        cache_write_tokens: None,
        cache_read_tokens: Some(u64::MAX),
        ..TokenUsage::default()
    };
    let next = TokenUsage {
        input_tokens: 10,
        output_tokens: u64::MAX,
        cache_write_tokens: Some(4),
        cache_read_tokens: None,
        ..TokenUsage::default()
    };

    total.saturating_add_assign(next);
    assert_eq!(total.input_tokens, u64::MAX);
    assert_eq!(total.output_tokens, u64::MAX);
    assert_eq!(total.cache_write_tokens, Some(4));
    assert_eq!(total.cache_read_tokens, Some(u64::MAX));
}

#[test]
fn agent_outcome_debug_impl() {
    let run = AgentOutcome {
        final_response: ModelResponse {
            id: "msg_x".into(),
            content: vec![],
            model: ModelRef::new("local", "test-model"),
            stop_reason: ProviderStopReason::EndTurn,
            usage: TokenUsage {
                input_tokens: 1,
                output_tokens: 1,
                ..TokenUsage::default()
            },
        },
        conversation: ConversationSnapshot::default(),
        iterations: 1,
        total_usage: TokenUsage {
            input_tokens: 1,
            output_tokens: 1,
            ..TokenUsage::default()
        },
    };
    let _ = format!("{run:?}");
    let _ = json!({});
}
