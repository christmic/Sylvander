use super::*;
use crate::execution::LocalExecutor;
use crate::test_support::qualified_anthropic_run_builder;
use std::path::PathBuf;
use sylvander_agent::approval::ToolApprovalFacts;
use sylvander_agent::approval::ToolUseRequest;
use sylvander_agent::compress::error::CompactionFailureCode;
use sylvander_agent::memory::store::InMemoryMemoryStore;
use sylvander_agent::tool::DynamicToolSource;
use sylvander_agent::tool::ToolExecutor as _;
use sylvander_agent::tool::invocation::ToolInvocationClass;
use sylvander_agent::tool::{ToolExecutionMode, ToolExecutionPolicy};
use sylvander_agent::tools::{ReadTool, WriteTool};
use sylvander_api::{Recipient, StreamEvent};
use sylvander_channel::{BusDiagnostics, BusError, InProcessMessageBus, MessageBus};
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_core::ModelInfo as ProviderModelInfo;

#[derive(Clone)]
struct SessionTestTool(&'static str);

impl sylvander_agent::tool::ToolDefinition for SessionTestTool {
    fn spec(&self) -> sylvander_agent::tool::ToolSpec {
        sylvander_agent::tool::ToolSpec::immediate(
            self.0,
            "Session test tool",
            serde_json::json!({"type": "object", "properties": {}}),
            sylvander_agent::tool::invocation::ToolInvocationClass::Extension,
        )
    }
}

#[async_trait::async_trait]
impl sylvander_agent::tool::ToolExecutor for SessionTestTool {
    async fn handle(
        &self,
        _context: &ToolContext,
        _call: &sylvander_agent::tool::PreparedToolCall,
    ) -> Result<sylvander_agent::tool::ToolOutput, sylvander_agent::tool::ToolError> {
        Ok(sylvander_agent::tool::ToolOutput::ok("ok"))
    }
}

#[derive(Clone)]
struct MutableSessionToolSource {
    tools: Arc<std::sync::RwLock<Vec<Arc<dyn RegisteredTool>>>>,
}

impl MutableSessionToolSource {
    fn new(name: &'static str) -> Self {
        Self {
            tools: Arc::new(std::sync::RwLock::new(vec![Arc::new(SessionTestTool(
                name,
            ))])),
        }
    }

    fn replace(&self, name: &'static str) {
        *self.tools.write().expect("dynamic source write lock") =
            vec![Arc::new(SessionTestTool(name))];
    }
}

impl DynamicToolSource for MutableSessionToolSource {
    fn snapshot(&self) -> Vec<Arc<dyn RegisteredTool>> {
        self.tools.read().expect("dynamic source read lock").clone()
    }
}

fn registry_gateway_factory() -> SessionInvocationGatewayFactory {
    Arc::new(|descriptors| {
        let gateway: Arc<dyn ToolInvocationGateway> =
            sylvander_agent::tool::invocation::RegistryBoundToolGateway::new(descriptors);
        Ok(gateway)
    })
}

struct TerminalOrderBus {
    inner: InProcessMessageBus,
    observability: crate::observability::RuntimeObservability,
}

#[async_trait::async_trait]
impl MessageBus for TerminalOrderBus {
    async fn publish(&self, message: BusMessage) -> Result<(), BusError> {
        if matches!(
            message.kind,
            MessageKind::Stream(sylvander_api::StreamEvent::Done { .. })
        ) {
            assert_eq!(self.observability.snapshot().turns_completed, 1);
        }
        self.inner.publish(message).await
    }

    async fn subscribe(
        &self,
        filter: SubscriptionFilter,
    ) -> Result<mpsc::Receiver<BusMessage>, BusError> {
        self.inner.subscribe(filter).await
    }

    async fn diagnostics(&self) -> BusDiagnostics {
        self.inner.diagnostics().await
    }
}

#[allow(clippy::too_many_arguments)]
async fn with_workspace_context(
    prompt: String,
    agent_workspace: Option<&sylvander_api::SessionWorkspaceBinding>,
    task_workspace: Option<&sylvander_api::SessionWorkspaceBinding>,
    workspace_mounts: &[sylvander_api::SessionWorkspaceMount],
    fallback_task_workspace: &Path,
    execution_service: &crate::execution::RuntimeExecutionService,
    skill_features: &std::sync::RwLock<Vec<sylvander_api::PlatformFeature>>,
) -> Result<String, AgentRunError> {
    let workspace = workspace_turn_context(
        agent_workspace,
        task_workspace,
        workspace_mounts,
        fallback_task_workspace,
        execution_service,
        skill_features,
        "",
        TurnContextBudgets::default().workspace_knowledge,
    )
    .await?;
    Ok(workspace.authoritative.map_or(prompt.clone(), |context| {
        format!("{prompt}\n\n{}", context.content())
    }))
}

impl AgentRun {
    async fn join_session(&self, meta: SessionMetadata) -> SessionId {
        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let ctx = SessionContext::new(session_id.clone(), meta);
        self.inner
            .sessions
            .write()
            .await
            .insert(session_id.clone(), ctx);
        self.inner
            .authenticated_sessions
            .write()
            .await
            .insert(session_id.clone());
        session_id
    }

    fn authenticated_session_for_test(&self, session_id: SessionId) -> AuthenticatedSession {
        AuthenticatedSession {
            authority: self.inner.session_authority.clone(),
            session_id,
        }
    }
}

fn direct_turn(
    run: &AgentRun,
    model: ProviderModelInfo,
    messages: Vec<ChatMessage>,
) -> (
    sylvander_agent::turn::request::AgentTurnRequest,
    sylvander_agent::execution_ports::AgentExecutionPorts,
) {
    let execution = AgentExecutionContext::restricted_for(
        "test-user",
        "router-agent",
        "direct-model-selection-test",
    );
    let request = sylvander_agent::turn::request::AgentTurnRequest {
        conversation: sylvander_agent::turn::conversation::ConversationSnapshot::new(messages),
        model,
        system_instructions: Vec::new(),
        reasoning: None,
        tools: run.inner.tools.clone(),
        execution: execution.clone(),
    };
    let gateway = run.inner.invocation_gateway.clone();
    let snapshot =
        sylvander_agent::tool::invocation::ToolInvocationGateway::snapshot(gateway.as_ref());
    let ports = sylvander_agent::execution_ports::AgentExecutionPorts::new(
        run.inner.model_provider.clone(),
        ToolContext::new(execution),
        gateway,
        snapshot,
    );
    (request, ports)
}

#[test]
fn turn_instructions_follow_the_exact_restricted_tool_catalog() {
    let model = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("anthropic", "test-model"),
        context_window: 100_000,
        max_output_tokens: 4_096,
        capabilities: sylvander_llm_core::ModelCapabilities::PROMPT_CACHING,
    };
    let tools = ToolRegistry::new()
        .register(ReadTool::new())
        .register(WriteTool::new())
        .register(MemoryReadTool::new(Arc::new(InMemoryMemoryStore::new())));
    let restricted = tools.retain_named(&[ReadTool::NAME, MemoryReadTool::NAME]);

    let instructions = turn_system_instructions("base", &model, &restricted);

    assert_eq!(instructions.len(), 2);
    assert_eq!(instructions[0].text, "base");
    assert_eq!(instructions[0].cache_hint, Some(CacheHint::Ephemeral));
    assert!(instructions[1].text.contains("[Read]"));
    assert!(instructions[1].text.contains("[read_memory]"));
    assert!(!instructions[1].text.contains("[Write]"));
    assert_eq!(instructions[1].cache_hint, Some(CacheHint::Ephemeral));
}

#[tokio::test]
async fn session_tool_surface_is_exact_and_removed_on_leave() {
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .override_tools(ToolRegistry::new().register(SessionTestTool("read")))
        .build()
        .expect("build run");
    let extensions = ToolRegistry::new().register(SessionTestTool("mcp__search__query"));
    let session_id = SessionId::new("session-tools");

    run.install_session_tool_extensions(session_id.clone(), extensions, registry_gateway_factory())
        .await
        .expect("install exact surface");
    let installed = run
        .inner
        .session_tool_surfaces
        .read()
        .await
        .get(&session_id)
        .cloned()
        .expect("installed surface");
    let tools = run
        .compose_session_tools(&installed.extensions)
        .expect("compose installed Session tools");
    assert!(tools.get("read").is_some());
    assert!(tools.get("mcp__search__query").is_some());

    run.leave_session(&session_id).await;
    assert!(
        !run.inner
            .session_tool_surfaces
            .read()
            .await
            .contains_key(&session_id)
    );
}

#[tokio::test]
async fn session_tool_surface_rejects_gateway_drift() {
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build()
        .expect("build run");
    let tools = ToolRegistry::new().register(SessionTestTool("mcp__search__query"));
    let empty_gateway_factory: SessionInvocationGatewayFactory = Arc::new(|_| {
        let gateway: Arc<dyn ToolInvocationGateway> =
            sylvander_agent::tool::invocation::RegistryBoundToolGateway::new(Vec::new());
        Ok(gateway)
    });

    let error = run
        .install_session_tool_extensions(SessionId::new("drift"), tools, empty_gateway_factory)
        .await
        .expect_err("mismatched gateway must fail");
    assert!(error.to_string().contains("authorization gateway differ"));
}

#[tokio::test]
async fn admitted_turn_uses_only_its_session_tool_snapshot() {
    let (spec, _) = test_spec_and_client();
    let provider = Arc::new(RecordingProvider::default());
    let model = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new(
            spec.model.provider.clone(),
            spec.model.model_name.clone(),
        ),
        context_window: 100_000,
        max_output_tokens: 4_096,
        capabilities: sylvander_llm_core::ModelCapabilities::TOOL_USE,
    };
    let run = AgentRun::qualified_router_builder(spec, provider.clone(), model)
        .bus(Arc::new(InProcessMessageBus::new()))
        .override_tools(ToolRegistry::new().register(SessionTestTool("read")))
        .build()
        .expect("build run");
    let first = run.join_session(test_metadata()).await;
    let second = run.join_session(test_metadata()).await;
    let extensions = ToolRegistry::new().register(SessionTestTool("mcp__search__query"));
    run.install_session_tool_extensions(first.clone(), extensions, registry_gateway_factory())
        .await
        .expect("install first Session tools");

    run.handle_message(BusMessage::user_chat(first, "user-1", "first Session"))
        .await
        .expect("first turn");
    run.handle_message(BusMessage::user_chat(second, "user-1", "second Session"))
        .await
        .expect("second turn");

    let requests = provider.requests.lock().expect("request lock");
    let first_names = requests[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    let second_names = requests[1]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert!(first_names.contains(&"mcp__search__query"));
    assert!(!second_names.contains(&"mcp__search__query"));
}

#[tokio::test]
async fn next_turn_uses_refreshed_session_tool_catalog() {
    let (spec, _) = test_spec_and_client();
    let provider = Arc::new(RecordingProvider::default());
    let model = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new(
            spec.model.provider.clone(),
            spec.model.model_name.clone(),
        ),
        context_window: 100_000,
        max_output_tokens: 4_096,
        capabilities: sylvander_llm_core::ModelCapabilities::TOOL_USE,
    };
    let run = AgentRun::qualified_router_builder(spec, provider.clone(), model)
        .bus(Arc::new(InProcessMessageBus::new()))
        .override_tools(ToolRegistry::new().register(SessionTestTool("read")))
        .build()
        .expect("build run");
    let session_id = run.join_session(test_metadata()).await;
    let source = MutableSessionToolSource::new("mcp__search__v1");
    let extensions = ToolRegistry::new().register_dynamic_source(source.clone());
    run.install_session_tool_extensions(session_id.clone(), extensions, registry_gateway_factory())
        .await
        .expect("install dynamic Session tools");

    run.handle_message(BusMessage::user_chat(
        session_id.clone(),
        "user-1",
        "first turn",
    ))
    .await
    .expect("first turn");
    source.replace("mcp__search__v2");
    run.handle_message(BusMessage::user_chat(session_id, "user-1", "second turn"))
        .await
        .expect("second turn");

    let requests = provider.requests.lock().expect("request lock");
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "mcp__search__v1")
    );
    assert!(
        !requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "mcp__search__v2")
    );
    assert!(
        requests[1]
            .tools
            .iter()
            .any(|tool| tool.name == "mcp__search__v2")
    );
    assert!(
        !requests[1]
            .tools
            .iter()
            .any(|tool| tool.name == "mcp__search__v1")
    );
}

#[test]
fn approval_rejection_reason_is_trimmed_bounded_and_optional() {
    assert_eq!(normalize_rejection_reason(None), "rejected by user");
    assert_eq!(
        normalize_rejection_reason(Some("  \n ")),
        "rejected by user"
    );
    assert_eq!(
        normalize_rejection_reason(Some("  unsafe outside workspace  ")),
        "unsafe outside workspace"
    );
    assert_eq!(
        normalize_rejection_reason(Some(&"x".repeat(501))).len(),
        500
    );
}

async fn next_stream_event(receiver: &mut mpsc::Receiver<BusMessage>) -> StreamEvent {
    loop {
        let message = receiver.recv().await.expect("stream event");
        if let MessageKind::Stream(event) = message.kind {
            return event;
        }
    }
}

fn test_metadata() -> SessionMetadata {
    SessionMetadata {
        workspace: PathBuf::from("/tmp/sylvander-test"),
        name: "test-session".into(),
        user_id: "user-1".into(),
    }
}

fn test_spec_and_client() -> (AgentSpec, AnthropicClient) {
    let spec = AgentSpec::builder()
        .id("test-agent")
        .name("Test")
        .model_name("claude-sonnet-5-20260601")
        .build()
        .expect("spec");
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .expect("client");
    (spec, client)
}

#[tokio::test]
async fn turn_prompt_contains_discovered_agent_task_and_skill_context() {
    let agent_home = tempfile::TempDir::new().unwrap();
    let task = tempfile::TempDir::new().unwrap();
    std::fs::write(agent_home.path().join("AGENTS.md"), "agent-home-guide").unwrap();
    std::fs::write(task.path().join("agent.md"), "task-guide").unwrap();
    std::fs::create_dir_all(task.path().join("src/api")).unwrap();
    std::fs::write(task.path().join("src/api/AGENTS.md"), "focused-task-guide").unwrap();
    std::fs::create_dir_all(task.path().join(".agents/skills/test")).unwrap();
    std::fs::write(
        task.path().join(".agents/skills/test/SKILL.md"),
        "skill-guide",
    )
    .unwrap();

    let execution_service = crate::execution::RuntimeExecutionService::new_for_test([(
        "local".to_owned(),
        Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>,
    )])
    .unwrap();
    let skill_features = std::sync::RwLock::new(Vec::new());
    let mounts = vec![sylvander_api::SessionWorkspaceMount {
        reference: "docs".into(),
        role: sylvander_api::WorkspaceMountRole::Dependency,
        binding: sylvander_api::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: task.path().into(),
            read_only: true,
            instruction_focus: None,
        },
        capabilities: sylvander_api::WorkspaceCapabilityPolicy {
            read: true,
            git: true,
            ..Default::default()
        },
    }];
    let prompt = with_workspace_context(
        "base-prompt".into(),
        Some(&sylvander_api::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: agent_home.path().to_path_buf(),
            read_only: true,
            instruction_focus: None,
        }),
        Some(&sylvander_api::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: task.path().to_path_buf(),
            read_only: false,
            instruction_focus: Some("src/api".into()),
        }),
        &mounts,
        task.path(),
        &execution_service,
        &skill_features,
    )
    .await
    .unwrap();
    let base = prompt.find("base-prompt").unwrap();
    let agent = prompt.find("agent-home-guide").unwrap();
    let skills = skill_features.read().unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "test");
    assert_eq!(
        skills[0].trust,
        Some(sylvander_api::PlatformTrust::Workspace)
    );
    assert_eq!(
        skills[0].status,
        sylvander_api::PlatformFeatureStatus::Active
    );
    assert!(
        skills[0]
            .capabilities
            .contains(&"prompt_instructions".to_owned())
    );
    assert!(skills[0].reloadable);
    let task = prompt.find("task-guide").unwrap();
    let skill = prompt.find("skill-guide").unwrap();
    let focused = prompt.find("focused-task-guide").unwrap();
    assert!(base < agent && agent < task && task < focused && focused < skill);
    assert!(prompt.contains("@docs (dependency): read, git"));
    assert!(prompt.contains("`@reference/path`"));
}

#[derive(Default)]
struct RecordingProvider {
    requests: std::sync::Mutex<Vec<sylvander_llm_core::ModelRequest>>,
}

#[derive(Clone)]
struct FixedUserProfile(sylvander_agent::user_profile::UserProfileSnapshot);

fn profile_with_learning(do_not_learn: bool) -> sylvander_agent::user_profile::UserProfileSnapshot {
    sylvander_agent::user_profile::UserProfileSnapshot {
        revision: 1,
        profile: sylvander_agent::user_profile::UserProfileData::default(),
        do_not_learn,
    }
}

#[async_trait::async_trait]
impl sylvander_agent::user_profile_provider::UserProfileProvider for FixedUserProfile {
    async fn current_profile(
        &self,
        _subject: &sylvander_agent::user_profile_provider::UserProfileSubject,
    ) -> Result<
        Option<sylvander_agent::user_profile::UserProfileSnapshot>,
        sylvander_agent::user_profile_provider::UserProfileProviderError,
    > {
        Ok(Some(self.0.clone()))
    }
}

struct UnavailableUserProfile;

#[async_trait::async_trait]
impl sylvander_agent::user_profile_provider::UserProfileProvider for UnavailableUserProfile {
    async fn current_profile(
        &self,
        _subject: &sylvander_agent::user_profile_provider::UserProfileSubject,
    ) -> Result<
        Option<sylvander_agent::user_profile::UserProfileSnapshot>,
        sylvander_agent::user_profile_provider::UserProfileProviderError,
    > {
        Err(sylvander_agent::user_profile_provider::UserProfileProviderError::Unavailable)
    }
}

#[derive(Debug)]
struct MarkerWorkspaceExecutor {
    marker: &'static [u8],
    reads: std::sync::Mutex<Vec<WorkspaceTarget>>,
}

impl MarkerWorkspaceExecutor {
    fn new(marker: &'static [u8]) -> Self {
        Self {
            marker,
            reads: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl WorkspaceExecutor for MarkerWorkspaceExecutor {
    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        _relative_path: &str,
    ) -> Result<Vec<u8>, sylvander_agent::workspace_executor::WorkspaceExecutorError> {
        self.reads.lock().unwrap().push(target.clone());
        Ok(self.marker.to_vec())
    }

    async fn write_file(
        &self,
        _target: &WorkspaceTarget,
        _relative_path: &str,
        _content: &[u8],
    ) -> Result<(), sylvander_agent::workspace_executor::WorkspaceExecutorError> {
        Ok(())
    }

    async fn run_command(
        &self,
        _target: &WorkspaceTarget,
        _command: &str,
        _timeout: std::time::Duration,
    ) -> Result<
        sylvander_agent::workspace_executor::WorkspaceCommandOutput,
        sylvander_agent::workspace_executor::WorkspaceExecutorError,
    > {
        Ok(
            sylvander_agent::workspace_executor::WorkspaceCommandOutput {
                success: true,
                status_code: Some(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                stdout_total_bytes: 0,
                stderr_total_bytes: 0,
            },
        )
    }

    async fn list(
        &self,
        _target: &WorkspaceTarget,
        request: sylvander_agent::workspace_executor::WorkspaceListRequest,
    ) -> Result<
        sylvander_agent::workspace_executor::WorkspaceListResult,
        sylvander_agent::workspace_executor::WorkspaceExecutorError,
    > {
        let entries = (request.relative_path == ".")
            .then(|| sylvander_agent::workspace_executor::WorkspaceListEntry {
                relative_path: "AGENTS.md".into(),
                kind: sylvander_agent::workspace_executor::WorkspaceEntryKind::File,
                size: self.marker.len() as u64,
            })
            .into_iter()
            .collect();
        Ok(sylvander_agent::workspace_executor::WorkspaceListResult {
            entries,
            truncated: false,
        })
    }
}

#[tokio::test]
async fn workspace_prompt_uses_each_execution_target_without_local_filesystem_access() {
    let agent = Arc::new(MarkerWorkspaceExecutor::new(b"remote-agent-guide"));
    let task = Arc::new(MarkerWorkspaceExecutor::new(b"remote-task-guide"));
    let execution_service = crate::execution::RuntimeExecutionService::new_for_test([
        (
            "ssh:agent".to_owned(),
            agent.clone() as Arc<dyn WorkspaceExecutor>,
        ),
        (
            "ssh:task".to_owned(),
            task.clone() as Arc<dyn WorkspaceExecutor>,
        ),
    ])
    .unwrap();
    let prompt = with_workspace_context(
        "base".into(),
        Some(&sylvander_api::SessionWorkspaceBinding {
            execution_target: "ssh:agent".into(),
            path: "/remote/agent".into(),
            read_only: true,
            instruction_focus: None,
        }),
        Some(&sylvander_api::SessionWorkspaceBinding {
            execution_target: "ssh:task".into(),
            path: "/remote/task".into(),
            read_only: false,
            instruction_focus: None,
        }),
        &[],
        Path::new("/attached/task"),
        &execution_service,
        &std::sync::RwLock::new(Vec::new()),
    )
    .await
    .unwrap();

    assert!(prompt.contains("remote-agent-guide"));
    assert!(prompt.contains("remote-task-guide"));
    assert_eq!(
        agent.reads.lock().unwrap()[0].workspace_path,
        Path::new("/remote/agent")
    );
    assert_eq!(
        task.reads.lock().unwrap()[0].workspace_path,
        Path::new("/remote/task")
    );
}

fn remote_effective_config(
    target_id: &str,
    workspace: &str,
) -> sylvander_api::SessionEffectiveConfig {
    let source = || sylvander_api::SessionConfigSource {
        kind: sylvander_api::SessionConfigSourceKind::RequestOverride,
        reference: None,
    };
    sylvander_api::SessionEffectiveConfig {
        agent_id: AgentId::new("test-agent"),
        agent_revision: 1,
        provider_id: "test".into(),
        provider_revision: 1,
        model_id: "test".into(),
        model_revision: 1,
        reasoning_effort: sylvander_api::ReasoningEffort::Off,
        permissions: sylvander_api::PermissionProfile::default(),
        prompt_profile: None,
        system_prompt_sha256: String::new(),
        prompt_manifest: sylvander_api::PromptManifest {
            layers: Vec::new(),
            aggregate_sha256: String::new(),
            total_bytes: 0,
        },
        agent_workspace: None,
        user_workspace: Some(sylvander_api::SessionWorkspaceBinding {
            execution_target: target_id.into(),
            path: workspace.into(),
            read_only: false,
            instruction_focus: None,
        }),
        workspace_mounts: Vec::new(),
        execution_target: target_id.into(),
        provenance: sylvander_api::SessionConfigProvenance {
            model: source(),
            reasoning_effort: source(),
            permissions: source(),
            prompt_profile: source(),
            system_prompt: source(),
            agent_workspace: source(),
            user_workspace: source(),
            execution_target: source(),
        },
    }
}

impl sylvander_llm_core::ModelProvider for RecordingProvider {
    fn complete_stream(
        &self,
        request: sylvander_llm_core::ModelRequest,
    ) -> sylvander_llm_core::ProviderFuture<'_> {
        self.requests.lock().unwrap().push(request.clone());
        Box::pin(async move {
            let response = sylvander_llm_core::ModelResponse {
                id: request.request_id,
                model: request.model,
                content: vec![sylvander_llm_core::ContentBlock::Text { text: "ok".into() }],
                stop_reason: sylvander_llm_core::StopReason::EndTurn,
                usage: sylvander_llm_core::TokenUsage::default(),
            };
            Ok(Box::pin(futures_util::stream::iter([Ok(
                sylvander_llm_core::ModelStreamEvent::Completed(Box::new(response)),
            )])) as sylvander_llm_core::ModelEventStream)
        })
    }
}

struct ToolCallingProvider;

impl sylvander_llm_core::ModelProvider for ToolCallingProvider {
    fn complete_stream(
        &self,
        request: sylvander_llm_core::ModelRequest,
    ) -> sylvander_llm_core::ProviderFuture<'_> {
        Box::pin(async move {
            let has_tool_result = request.messages.iter().any(|message| {
                message.content.iter().any(|block| {
                    matches!(block, sylvander_llm_core::ContentBlock::ToolResult { .. })
                })
            });
            let (content, stop_reason) = if has_tool_result {
                (
                    vec![sylvander_llm_core::ContentBlock::Text {
                        text: "done".into(),
                    }],
                    sylvander_llm_core::StopReason::EndTurn,
                )
            } else {
                (
                    vec![sylvander_llm_core::ContentBlock::ToolCall {
                        id: "durable-call".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({}),
                    }],
                    sylvander_llm_core::StopReason::ToolUse,
                )
            };
            let response = sylvander_llm_core::ModelResponse {
                id: request.request_id,
                model: request.model,
                content,
                stop_reason,
                usage: sylvander_llm_core::TokenUsage::default(),
            };
            Ok(Box::pin(futures_util::stream::iter([Ok(
                sylvander_llm_core::ModelStreamEvent::Completed(Box::new(response)),
            )])) as sylvander_llm_core::ModelEventStream)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionStoreFailPoint {
    Get,
    Save,
    ReadHistory,
    BeginTurn,
    BeginToolCall,
    FinishToolCall,
    RecordUsage,
    AppendMessage,
    ReplaceHistory,
}

struct FailingSessionStore {
    inner: Arc<dyn SessionStore>,
    fail: SessionStoreFailPoint,
}

impl FailingSessionStore {
    fn new(inner: Arc<dyn SessionStore>, fail: SessionStoreFailPoint) -> Self {
        Self { inner, fail }
    }

    fn injected() -> crate::storage::session::SessionStoreError {
        crate::storage::session::SessionStoreError::Store("private injected detail".into())
    }
}

#[async_trait::async_trait]
impl SessionStore for FailingSessionStore {
    async fn list_persistent(
        &self,
    ) -> Result<Vec<StoredSession>, crate::storage::session::SessionStoreError> {
        self.inner.list_persistent().await
    }

    async fn save(
        &self,
        session: &StoredSession,
    ) -> Result<(), crate::storage::session::SessionStoreError> {
        if self.fail == SessionStoreFailPoint::Save {
            return Err(Self::injected());
        }
        self.inner.save(session).await
    }

    async fn patch_metadata(
        &self,
        id: &SessionId,
        patch: crate::storage::session::SessionMetadataPatch,
    ) -> Result<(), crate::storage::session::SessionStoreError> {
        self.inner.patch_metadata(id, patch).await
    }

    async fn update_config(
        &self,
        id: &SessionId,
        expected_revision: u64,
        overrides: sylvander_api::SessionConfigOverrides,
        effective: sylvander_api::SessionEffectiveConfig,
    ) -> Result<u64, crate::storage::session::SessionStoreError> {
        self.inner
            .update_config(id, expected_revision, overrides, effective)
            .await
    }

    async fn begin_turn(
        &self,
        context: &sylvander_api::SessionContext,
        start: TurnStart,
    ) -> Result<crate::storage::session::StoredMessage, crate::storage::session::SessionStoreError>
    {
        if self.fail == SessionStoreFailPoint::BeginTurn {
            return Err(Self::injected());
        }
        self.inner.begin_turn(context, start).await
    }

    async fn turn(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<
        Option<crate::storage::session::TurnSnapshot>,
        crate::storage::session::SessionStoreError,
    > {
        self.inner.turn(session_id, turn_id).await
    }

    async fn complete_turn(
        &self,
        context: &sylvander_api::SessionContext,
        completion: crate::storage::session::TurnCompletion,
    ) -> Result<crate::storage::session::StoredMessage, crate::storage::session::SessionStoreError>
    {
        if self.fail == SessionStoreFailPoint::AppendMessage {
            return Err(Self::injected());
        }
        self.inner.complete_turn(context, completion).await
    }

    async fn finish_turn(
        &self,
        session_id: &SessionId,
        turn_id: &str,
        state: crate::storage::session::TurnState,
        failure_kind: Option<crate::storage::session::TurnFailureKind>,
    ) -> Result<(), crate::storage::session::SessionStoreError> {
        self.inner
            .finish_turn(session_id, turn_id, state, failure_kind)
            .await
    }

    async fn begin_tool_call(
        &self,
        start: crate::storage::session::ToolCallStart,
    ) -> Result<(), crate::storage::session::SessionStoreError> {
        if self.fail == SessionStoreFailPoint::BeginToolCall {
            return Err(Self::injected());
        }
        self.inner.begin_tool_call(start).await
    }

    async fn finish_tool_call(
        &self,
        completion: crate::storage::session::ToolCallCompletion,
    ) -> Result<(), crate::storage::session::SessionStoreError> {
        if self.fail == SessionStoreFailPoint::FinishToolCall {
            return Err(Self::injected());
        }
        self.inner.finish_tool_call(completion).await
    }

    async fn tool_calls(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<
        Vec<crate::storage::session::ToolCallSnapshot>,
        crate::storage::session::SessionStoreError,
    > {
        self.inner.tool_calls(session_id, turn_id).await
    }

    async fn archive(
        &self,
        id: &SessionId,
    ) -> Result<(), crate::storage::session::SessionStoreError> {
        self.inner.archive(id).await
    }

    async fn restore(
        &self,
        id: &SessionId,
    ) -> Result<(), crate::storage::session::SessionStoreError> {
        self.inner.restore(id).await
    }

    async fn record_usage(
        &self,
        id: &SessionId,
        input_tokens: u32,
        output_tokens: u32,
        cost_nano_usd: Option<u64>,
    ) -> Result<crate::storage::session::SessionUsage, crate::storage::session::SessionStoreError>
    {
        if self.fail == SessionStoreFailPoint::RecordUsage {
            return Err(Self::injected());
        }
        self.inner
            .record_usage(id, input_tokens, output_tokens, cost_nano_usd)
            .await
    }

    async fn usage(
        &self,
        id: &SessionId,
    ) -> Result<crate::storage::session::SessionUsage, crate::storage::session::SessionStoreError>
    {
        self.inner.usage(id).await
    }

    async fn delete(
        &self,
        id: &SessionId,
    ) -> Result<(), crate::storage::session::SessionStoreError> {
        self.inner.delete(id).await
    }

    async fn get(
        &self,
        id: &SessionId,
    ) -> Result<Option<StoredSession>, crate::storage::session::SessionStoreError> {
        if self.fail == SessionStoreFailPoint::Get {
            return Err(Self::injected());
        }
        self.inner.get(id).await
    }

    async fn get_including_archived(
        &self,
        id: &SessionId,
    ) -> Result<Option<StoredSession>, crate::storage::session::SessionStoreError> {
        self.inner.get_including_archived(id).await
    }

    async fn list(
        &self,
        context: &sylvander_api::SessionContext,
        filter: crate::storage::session::SessionFilter,
    ) -> Result<Vec<StoredSession>, crate::storage::session::SessionStoreError> {
        self.inner.list(context, filter).await
    }

    async fn search(
        &self,
        context: &sylvander_api::SessionContext,
        query: &str,
        limit: usize,
    ) -> Result<Vec<StoredSession>, crate::storage::session::SessionStoreError> {
        self.inner.search(context, query, limit).await
    }

    async fn append_message(
        &self,
        context: &sylvander_api::SessionContext,
        session_id: &SessionId,
        role: StoredMessageRole,
        message_content: serde_json::Value,
        model_id: Option<&str>,
        tool_name: Option<&str>,
        parent_msg_id: Option<i64>,
    ) -> Result<crate::storage::session::StoredMessage, crate::storage::session::SessionStoreError>
    {
        if self.fail == SessionStoreFailPoint::AppendMessage {
            return Err(Self::injected());
        }
        self.inner
            .append_message(
                context,
                session_id,
                role,
                message_content,
                model_id,
                tool_name,
                parent_msg_id,
            )
            .await
    }

    async fn read_history(
        &self,
        context: &sylvander_api::SessionContext,
        session_id: &SessionId,
        include_summarized: bool,
        limit: Option<usize>,
    ) -> Result<
        Vec<crate::storage::session::StoredMessage>,
        crate::storage::session::SessionStoreError,
    > {
        if self.fail == SessionStoreFailPoint::ReadHistory {
            return Err(Self::injected());
        }
        self.inner
            .read_history(context, session_id, include_summarized, limit)
            .await
    }

    async fn mark_summarized(
        &self,
        session_id: &SessionId,
        seq_range: std::ops::Range<u32>,
    ) -> Result<(), crate::storage::session::SessionStoreError> {
        self.inner.mark_summarized(session_id, seq_range).await
    }

    async fn replace_active_history(
        &self,
        context: &sylvander_api::SessionContext,
        session_id: &SessionId,
        messages: Vec<ReplacementMessage>,
    ) -> Result<(), crate::storage::session::SessionStoreError> {
        if self.fail == SessionStoreFailPoint::ReplaceHistory {
            return Err(Self::injected());
        }
        self.inner
            .replace_active_history(context, session_id, messages)
            .await
    }

    async fn count_active_messages(
        &self,
        context: &sylvander_api::SessionContext,
        session_id: &SessionId,
    ) -> Result<u64, crate::storage::session::SessionStoreError> {
        self.inner.count_active_messages(context, session_id).await
    }
}

async fn persistent_tool_lifecycle(
    approval_policy: sylvander_api::ApprovalPolicy,
) -> (
    Result<(), AgentRunError>,
    crate::RuntimeObservabilitySnapshot,
    Vec<StreamEvent>,
) {
    persistent_tool_lifecycle_with_failure(approval_policy, None).await
}

async fn persistent_tool_lifecycle_with_failure(
    approval_policy: sylvander_api::ApprovalPolicy,
    fail: Option<SessionStoreFailPoint>,
) -> (
    Result<(), AgentRunError>,
    crate::RuntimeObservabilitySnapshot,
    Vec<StreamEvent>,
) {
    let inner: Arc<dyn SessionStore> = Arc::new(
        crate::storage::session::SqliteSessionStore::open_in_memory()
            .await
            .unwrap(),
    );
    let store: Arc<dyn SessionStore> = fail.map_or_else(
        || inner.clone(),
        |point| Arc::new(FailingSessionStore::new(inner.clone(), point)),
    );
    let (spec, _) = test_spec_and_client();
    let resolver = Arc::new(
        sylvander_agent::prompt::PromptResolver::new(
            "agent:test-agent@1".into(),
            spec.persona.system_prompt.clone(),
            Vec::new(),
            None,
            false,
        )
        .unwrap(),
    );
    let model = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new(
            spec.model.provider.clone(),
            spec.model.model_name.clone(),
        ),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: sylvander_llm_core::ModelCapabilities::TOOL_USE,
    };
    let (run, issuer) =
        AgentRun::qualified_router_builder(spec, Arc::new(ToolCallingProvider), model)
            .bus(Arc::new(InProcessMessageBus::new()))
            .session_store(store.clone())
            .prompt_resolver(resolver)
            .override_tools(ToolRegistry::new().register(SessionTestTool("read")))
            .build_with_session_issuer()
            .unwrap();
    let session_id = SessionId::new(format!("durable-tool-{approval_policy:?}"));
    let metadata = test_metadata();
    let mut stored = StoredSession::new(
        session_id.clone(),
        metadata.name.clone(),
        SessionLifetime::Persistent,
        metadata.clone(),
        vec![run.id().clone()],
    );
    stored.effective_config = Some(run.inner.direct_session_config(&metadata).await);
    stored
        .effective_config
        .as_mut()
        .unwrap()
        .permissions
        .approval_policy = approval_policy;
    inner.save(&stored).await.unwrap();
    let lease = issuer.issue(session_id.clone(), metadata.clone()).unwrap();
    run.attach_authenticated_session(lease).await.unwrap();
    let mut receiver = run
        .inner
        .bus
        .subscribe(SubscriptionFilter::all())
        .await
        .unwrap();

    let result = run
        .handle_message(BusMessage::user_chat(
            session_id,
            metadata.user_id,
            "use the tool",
        ))
        .await;
    let mut events = Vec::new();
    while let Ok(message) = receiver.try_recv() {
        if let MessageKind::Stream(event) = message.kind {
            events.push(event);
        }
    }

    (result, run.inner.observability.snapshot(), events)
}

#[tokio::test]
async fn persistent_agent_run_closes_executed_and_rejected_tool_lifecycles() {
    for (policy, succeeded, failed) in [
        (sylvander_api::ApprovalPolicy::Allow, 1, 0),
        (sylvander_api::ApprovalPolicy::Deny, 0, 1),
    ] {
        let (result, snapshot, events) = persistent_tool_lifecycle(policy).await;
        result.unwrap();
        assert!(matches!(
            events.first(),
            Some(StreamEvent::TurnStarted { .. })
        ));
        assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
        assert_eq!(snapshot.turns_completed, 1);
        assert_eq!(snapshot.tools_started, 1);
        assert_eq!(snapshot.tools_succeeded, succeeded);
        assert_eq!(snapshot.tools_failed, failed);
        assert_eq!(snapshot.persistence_succeeded, 6);
        assert_eq!(snapshot.persistence_failed, 0);
        assert_eq!(snapshot.active_tools, 0);
    }
}

#[tokio::test]
async fn durable_tool_persistence_failures_fail_the_turn_and_clear_active_work() {
    for (fail, operation, started) in [
        (
            SessionStoreFailPoint::BeginToolCall,
            SessionPersistenceOperation::BeginToolCall,
            0,
        ),
        (
            SessionStoreFailPoint::FinishToolCall,
            SessionPersistenceOperation::FinishToolCall,
            1,
        ),
    ] {
        let (result, snapshot, events) = persistent_tool_lifecycle_with_failure(
            sylvander_api::ApprovalPolicy::Allow,
            Some(fail),
        )
        .await;
        assert_persistence_failure(result.unwrap_err(), operation);
        assert!(matches!(
            events.first(),
            Some(StreamEvent::TurnStarted { .. })
        ));
        assert!(matches!(events.last(), Some(StreamEvent::Error { .. })));
        assert_eq!(snapshot.turns_completed, 0);
        assert_eq!(snapshot.turns_failed, 1);
        assert_eq!(snapshot.tools_started, started);
        assert_eq!(snapshot.persistence_failed, 1);
        assert_eq!(snapshot.active_tools, 0);
    }
}

#[tokio::test]
async fn durable_turn_prompt_uses_attached_workspace_instead_of_stale_binding() {
    let source = tempfile::TempDir::new().unwrap();
    let worktree = tempfile::TempDir::new().unwrap();
    std::fs::write(source.path().join("AGENTS.md"), "source-workspace-guide").unwrap();
    std::fs::write(
        worktree.path().join("AGENTS.md"),
        "effective-worktree-guide",
    )
    .unwrap();

    let store: Arc<dyn SessionStore> = Arc::new(
        crate::storage::session::SqliteSessionStore::open_in_memory()
            .await
            .unwrap(),
    );
    let (spec, _) = test_spec_and_client();
    let resolver = Arc::new(
        sylvander_agent::prompt::PromptResolver::new(
            "agent:test-agent@1".into(),
            spec.persona.system_prompt.clone(),
            Vec::new(),
            None,
            false,
        )
        .unwrap(),
    );
    let provider = Arc::new(RecordingProvider::default());
    let model = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new(
            spec.model.provider.clone(),
            spec.model.model_name.clone(),
        ),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let observability = crate::observability::RuntimeObservability::new();
    let run = AgentRun::qualified_router_builder(spec, provider.clone(), model)
        .bus(Arc::new(TerminalOrderBus {
            inner: InProcessMessageBus::new(),
            observability: observability.clone(),
        }))
        .observability(observability.clone())
        .session_store(store.clone())
        .prompt_resolver(resolver)
        .build()
        .unwrap();
    let metadata = SessionMetadata {
        workspace: worktree.path().to_path_buf(),
        ..test_metadata()
    };
    let session_id = run.join_session(metadata.clone()).await;
    let mut stored = StoredSession::new(
        session_id.clone(),
        metadata.name.clone(),
        SessionLifetime::Persistent,
        metadata.clone(),
        vec![run.id().clone()],
    );
    stored.effective_config = Some(run.inner.direct_session_config(&metadata).await);
    stored
        .effective_config
        .as_mut()
        .unwrap()
        .user_workspace
        .as_mut()
        .unwrap()
        .path = source.path().to_path_buf();
    store.save(&stored).await.unwrap();

    run.handle_message(BusMessage::user_chat(
        session_id,
        metadata.user_id,
        "inspect the workspace",
    ))
    .await
    .unwrap();

    let snapshot = observability.snapshot();
    assert_eq!(snapshot.turns_started, 1);
    assert_eq!(snapshot.turns_completed, 1);
    assert_eq!(snapshot.persistence_succeeded, 3);
    assert_eq!(snapshot.active_turns, 0);
    assert_eq!(snapshot.turn_latency.count, 1);

    let system = {
        let requests = provider.requests.lock().unwrap();
        requests[0]
            .system
            .iter()
            .map(|instruction| instruction.text.as_str())
            .collect::<String>()
    };
    assert!(system.contains("effective-worktree-guide"));
    assert!(!system.contains("source-workspace-guide"));
}

#[tokio::test]
async fn live_turn_injects_all_typed_context_layers_and_exposes_a_manifest() {
    let workspace = tempfile::TempDir::new().unwrap();
    std::fs::write(
        workspace.path().join("AGENTS.md"),
        "workspace instructions stay below runtime safety",
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("knowledge.md"),
        "typed context retrieval must stay bounded and relevant\n",
    )
    .unwrap();

    let memory = Arc::new(InMemoryMemoryStore::new());
    let memory_caller =
        AgentExecutionContext::restricted_for("user-1", "test-agent", "memory-seed");
    let memory_context = MemoryExecutionContext::for_runtime_worker(&memory_caller);
    memory
        .append_relationship(
            &memory_context,
            MemoryAppend::new("typed context should prefer relevant relationship memory"),
        )
        .await
        .unwrap();
    memory
        .append_relationship(
            &memory_context,
            MemoryAppend::new("unrelated favorite lunch"),
        )
        .await
        .unwrap();

    let store: Arc<dyn SessionStore> = Arc::new(
        crate::storage::session::SqliteSessionStore::open_in_memory()
            .await
            .unwrap(),
    );
    let (spec, _) = test_spec_and_client();
    let selection = sylvander_api::ModelSelection {
        provider_id: spec.model.provider.clone(),
        model_id: spec.model.model_name.clone(),
    };
    let resolver = Arc::new(
        sylvander_agent::prompt::PromptResolver::new(
            "agent:test-agent@3".into(),
            "agent persona".into(),
            Vec::new(),
            None,
            true,
        )
        .unwrap(),
    );
    let profile = sylvander_agent::user_profile::UserProfileSnapshot {
        revision: 9,
        profile: sylvander_agent::user_profile::UserProfileData {
            preferred_language: Some(sylvander_agent::user_profile::ClassifiedPreference {
                value: "zh-CN".into(),
                privacy_class: sylvander_agent::user_profile::PrivacyClass::Personal,
            }),
            ..sylvander_agent::user_profile::UserProfileData::default()
        },
        do_not_learn: false,
    };
    let provider = Arc::new(RecordingProvider::default());
    let model = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new(
            selection.provider_id.clone(),
            selection.model_id.clone(),
        ),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let run = AgentRun::qualified_router_builder(spec, provider.clone(), model)
        .bus(Arc::new(InProcessMessageBus::new()))
        .session_store(store.clone())
        .memory(memory)
        .prompt_resolver(resolver.clone())
        .user_profile_provider(Arc::new(FixedUserProfile(profile)))
        .build()
        .unwrap();
    let metadata = SessionMetadata {
        workspace: workspace.path().to_path_buf(),
        ..test_metadata()
    };
    let session_id = run.join_session(metadata.clone()).await;
    let authenticated = run.authenticated_session_for_test(session_id.clone());
    let mut stored = StoredSession::new(
        session_id.clone(),
        metadata.name.clone(),
        SessionLifetime::Persistent,
        metadata.clone(),
        vec![run.id().clone()],
    );
    stored.config_overrides.system_prompt = Some("respond with evidence".into());
    let prompt_snapshot = resolver
        .resolve(
            &agent_model_selection(&selection),
            None,
            Some("respond with evidence"),
        )
        .unwrap();
    let mut effective = run.inner.direct_session_config(&metadata).await;
    effective.agent_revision = 3;
    effective.system_prompt_sha256 = prompt_snapshot.system_prompt_sha256;
    effective.prompt_manifest = public_prompt_manifest(prompt_snapshot.manifest);
    stored.effective_config = Some(effective);
    store.save(&stored).await.unwrap();

    run.handle_message(BusMessage::user_chat(
        session_id,
        metadata.user_id,
        "explain typed context retrieval",
    ))
    .await
    .unwrap();

    let system = {
        let requests = provider.requests.lock().unwrap();
        requests[0]
            .system
            .iter()
            .map(|instruction| instruction.text.as_str())
            .collect::<String>()
    };
    let positions = [
        "kind=safety",
        "kind=agent",
        "kind=user_profile",
        "kind=relationship_memory",
        "kind=workspace_knowledge",
        "kind=session",
    ]
    .map(|marker| system.find(marker).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(system.contains("relevant relationship memory"));
    assert!(system.contains("knowledge.md:1"));
    assert!(system.contains("respond with evidence"));
    assert!(!system.contains("favorite lunch"));
    let manifest = run
        .turn_context_manifest(&authenticated)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        manifest.schema_version,
        sylvander_agent::turn_context::TURN_CONTEXT_SCHEMA_VERSION
    );
    assert_eq!(manifest.layers.len(), 6);
    assert_eq!(manifest.aggregate_sha256.len(), 64);
    assert!(
        manifest
            .layers
            .iter()
            .all(|layer| !layer.included_items.is_empty())
    );
}

#[tokio::test]
async fn identity_and_prompt_integrity_fail_before_provider_and_durable_turn_writes() {
    #[derive(Clone, Copy)]
    enum Tamper {
        SenderIdentity,
        SystemHash,
        LayerHash,
    }

    for tamper in [
        Tamper::SenderIdentity,
        Tamper::SystemHash,
        Tamper::LayerHash,
    ] {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = directory.path().join("sessions.db");
        let store: Arc<dyn SessionStore> = Arc::new(
            crate::storage::session::SqliteSessionStore::open(&database)
                .await
                .expect("store"),
        );
        let (spec, _) = test_spec_and_client();
        let selection = sylvander_api::ModelSelection {
            provider_id: spec.model.provider.clone(),
            model_id: spec.model.model_name.clone(),
        };
        let resolver = Arc::new(
            sylvander_agent::prompt::PromptResolver::new(
                "agent:test-agent@1".into(),
                spec.persona.system_prompt.clone(),
                Vec::new(),
                None,
                true,
            )
            .expect("prompt resolver"),
        );
        let prompt_snapshot = resolver
            .resolve(
                &agent_model_selection(&selection),
                None,
                Some("private prompt sentinel"),
            )
            .expect("resolved prompt");
        let provider = Arc::new(RecordingProvider::default());
        let model = ProviderModelInfo {
            reference: sylvander_llm_core::ModelRef::new(
                selection.provider_id.clone(),
                selection.model_id.clone(),
            ),
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities: sylvander_llm_core::ModelCapabilities::empty(),
        };
        let run = AgentRun::qualified_router_builder(spec, provider.clone(), model)
            .bus(Arc::new(InProcessMessageBus::new()))
            .session_store(store.clone())
            .prompt_resolver(resolver)
            .build()
            .expect("run");
        let metadata = test_metadata();
        let session_id = run.join_session(metadata.clone()).await;
        let mut stored = StoredSession::new(
            session_id.clone(),
            metadata.name.clone(),
            SessionLifetime::Persistent,
            metadata.clone(),
            vec![run.id().clone()],
        );
        stored.config_overrides.system_prompt = Some("private prompt sentinel".into());
        let mut effective = run.inner.direct_session_config(&metadata).await;
        effective.agent_revision = 1;
        effective.system_prompt_sha256 = prompt_snapshot.system_prompt_sha256;
        effective.prompt_manifest = public_prompt_manifest(prompt_snapshot.manifest);
        match tamper {
            Tamper::SenderIdentity => {}
            Tamper::SystemHash => effective.system_prompt_sha256 = "tampered".into(),
            Tamper::LayerHash => {
                effective.prompt_manifest.layers[0].sha256 = "tampered".into();
            }
        }
        stored.effective_config = Some(effective);
        store.save(&stored).await.expect("save tampered session");

        let error = run
            .handle_message(BusMessage::user_chat(
                session_id.clone(),
                if matches!(tamper, Tamper::SenderIdentity) {
                    "different-user"
                } else {
                    "user-1"
                },
                "must not execute",
            ))
            .await
            .expect_err("invalid session inputs must fail closed");
        let rendered = error.to_string();
        assert_eq!(
            rendered,
            if matches!(tamper, Tamper::SenderIdentity) {
                "session configuration error: session identity verification failed"
            } else {
                "session configuration error: prompt integrity verification failed"
            }
        );
        assert!(!rendered.contains("private prompt sentinel"));
        assert!(provider.requests.lock().unwrap().is_empty());

        let connection = rusqlite::Connection::open(&database).expect("inspect database");
        for table in ["session_turns", "session_messages"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("row count");
            assert_eq!(count, 0, "{table} must remain untouched");
        }
    }
}

#[tokio::test]
async fn provider_catalog_is_qualified_and_turn_snapshot_uses_exact_model() {
    let mut spec = AgentSpec::builder()
        .id("provider-agent")
        .name("Provider")
        .model_name("shared")
        .build()
        .unwrap();
    spec.model.provider = "local".into();
    let provider = Arc::new(RecordingProvider::default());
    let provider_model = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("local", "shared"),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let alternate = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("local", "model-b"),
        context_window: 200_000,
        max_output_tokens: 8192,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let foreign = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("remote", "shared"),
        context_window: 300_000,
        max_output_tokens: 16_384,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let run = AgentRun::qualified_router_builder(spec, provider.clone(), provider_model)
        .bus(Arc::new(InProcessMessageBus::new()))
        .available_provider_models(vec![alternate, foreign])
        .build()
        .unwrap();

    let before = run.runtime_model_info().await;
    assert_eq!(before.models.len(), 3);
    let remote_selection = sylvander_api::ModelSelection {
        provider_id: "remote".into(),
        model_id: "shared".into(),
    };
    run.select_qualified_model(
        remote_selection.clone(),
        sylvander_api::ReasoningEffort::Off,
    )
    .await
    .unwrap();
    assert_eq!(
        run.inner.runtime_models.read().await.current,
        remote_selection
    );
    run.select_qualified_model(
        sylvander_api::ModelSelection {
            provider_id: "local".into(),
            model_id: "model-b".into(),
        },
        sylvander_api::ReasoningEffort::Off,
    )
    .await
    .unwrap();
    let selected = {
        let runtime = run.inner.runtime_models.read().await;
        runtime.available.get(&runtime.current).unwrap().clone()
    };
    let selected =
        AgentRunInner::validate_turn_model(&selected, sylvander_api::ReasoningEffort::Off).unwrap();
    let (request, ports) = direct_turn(&run, selected, vec![ChatMessage::user("hello")]);
    sylvander_agent::kernel::agent_loop::run(&run.inner.loop_config, request, ports)
        .await
        .unwrap();
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].model,
        sylvander_llm_core::ModelRef::new("local", "model-b")
    );
}

#[tokio::test]
async fn qualified_router_crosses_providers_without_metadata_collisions() {
    let mut spec = AgentSpec::builder()
        .id("router-agent")
        .name("Router")
        .model_name("shared")
        .build()
        .unwrap();
    spec.model.provider = "local".into();
    let router = Arc::new(RecordingProvider::default());
    let local = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("local", "shared"),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let remote = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("remote", "shared"),
        context_window: 200_000,
        max_output_tokens: 8192,
        capabilities: sylvander_llm_core::ModelCapabilities::TOOL_USE
            | sylvander_llm_core::ModelCapabilities::VISION,
    };
    let local_selection = sylvander_api::ModelSelection {
        provider_id: "local".into(),
        model_id: "shared".into(),
    };
    let remote_selection = sylvander_api::ModelSelection {
        provider_id: "remote".into(),
        model_id: "shared".into(),
    };
    let remote_pricing = sylvander_api::ModelPricing {
        input_usd_micros_per_million: 11,
        output_usd_micros_per_million: 22,
        cache_write_usd_micros_per_million: None,
        cache_read_usd_micros_per_million: None,
    };
    let run = AgentRun::qualified_router_builder(spec, router.clone(), local)
        .bus(Arc::new(InProcessMessageBus::new()))
        .available_provider_models(vec![remote])
        .qualified_model_lifecycles(HashMap::from([
            (local_selection, sylvander_api::ModelLifecycle::Active),
            (
                remote_selection.clone(),
                sylvander_api::ModelLifecycle::Deprecated { replacement: None },
            ),
        ]))
        .qualified_model_pricing(HashMap::from([(remote_selection.clone(), remote_pricing)]))
        .build()
        .unwrap();

    let advertised = run.runtime_model_info().await;
    let local = advertised
        .models
        .iter()
        .find(|model| model.provider == "local" && model.id == "shared")
        .unwrap();
    let remote = advertised
        .models
        .iter()
        .find(|model| model.provider == "remote" && model.id == "shared")
        .unwrap();
    assert_eq!(local.lifecycle, sylvander_api::ModelLifecycle::Active);
    assert_eq!(local.pricing, None);
    assert!(matches!(
        remote.lifecycle,
        sylvander_api::ModelLifecycle::Deprecated { .. }
    ));
    assert_eq!(remote.pricing, Some(remote_pricing));
    assert_eq!(
        remote.capability_names,
        [
            sylvander_api::ModelCapability::ToolUse,
            sylvander_api::ModelCapability::Vision,
        ]
    );

    run.select_qualified_model(remote_selection, sylvander_api::ReasoningEffort::Off)
        .await
        .unwrap();
    let selected = {
        let runtime = run.inner.runtime_models.read().await;
        runtime.available.get(&runtime.current).unwrap().clone()
    };
    let selected =
        AgentRunInner::validate_turn_model(&selected, sylvander_api::ReasoningEffort::Off).unwrap();
    let (request, ports) = direct_turn(&run, selected, vec![ChatMessage::user("hello")]);
    sylvander_agent::kernel::agent_loop::run(&run.inner.loop_config, request, ports)
        .await
        .unwrap();
    assert_eq!(
        router.requests.lock().unwrap()[0].model,
        sylvander_llm_core::ModelRef::new("remote", "shared")
    );
}

#[tokio::test]
async fn provider_manual_compaction_uses_backend_factory() {
    let mut spec = AgentSpec::builder()
        .id("provider-agent")
        .name("Provider")
        .model_name("model-a")
        .build()
        .unwrap();
    spec.model.provider = "local".into();
    let provider = Arc::new(RecordingProvider::default());
    let run = AgentRun::qualified_router_builder(
        spec,
        provider.clone(),
        ProviderModelInfo {
            reference: sylvander_llm_core::ModelRef::new("local", "model-a"),
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities: sylvander_llm_core::ModelCapabilities::empty(),
        },
    )
    .bus(Arc::new(InProcessMessageBus::new()))
    .build()
    .unwrap();
    let session_id = run.join_session(test_metadata()).await;
    {
        let mut sessions = run.inner.sessions.write().await;
        let session = sessions.get_mut(&session_id).unwrap();
        for index in 0..6 {
            session.append_user_message(ChatMessage::user(format!("message {index}")));
        }
    }

    let report = run.compact_session(&session_id).await.unwrap();
    assert_eq!(report.removed_messages, 2);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert_eq!(run.get_session(&session_id).await.unwrap().len(), 5);
}

#[tokio::test]
async fn manual_compaction_failures_are_typed_before_string_facade() {
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build()
        .unwrap();
    let missing = SessionId::new("missing");
    assert_eq!(
        run.compact_session_typed(&missing).await.unwrap_err().code,
        CompactionFailureCode::SessionUnavailable
    );
    let session_id = run.join_session(test_metadata()).await;
    assert_eq!(
        run.compact_session_typed(&session_id)
            .await
            .unwrap_err()
            .code,
        CompactionFailureCode::InsufficientHistory
    );
    let (interrupt, _receiver) = oneshot::channel();
    run.inner.active_turns.lock().await.insert(
        session_id.clone(),
        ActiveTurn {
            id: uuid::Uuid::new_v4(),
            interrupt,
        },
    );
    assert_eq!(
        run.compact_session_typed(&session_id)
            .await
            .unwrap_err()
            .code,
        CompactionFailureCode::Busy
    );
}

#[test]
fn turn_correlation_keeps_request_and_trace_boundaries_explicit() {
    let message = BusMessage::user_chat(SessionId::new("session"), "user", "hello");
    let request_id = message.id.0.to_string();
    let turn_id = uuid::Uuid::parse_str("13fcf8b4-31f8-4b3a-9432-0cc9ad73d7c0").unwrap();

    let correlation = TurnCorrelation::new(&message, turn_id);

    assert_eq!(correlation.request, request_id);
    assert_eq!(correlation.turn, turn_id.to_string());
    assert_eq!(correlation.trace, correlation.turn);
}

#[test]
fn platform_snapshot_is_truthful_and_redacts_configuration_secrets() {
    let spec = AgentSpec::builder()
        .id("test-agent")
        .name("Test")
        .model_name("test-model")
        .mcp_server(crate::agent_definition::McpServerConfig {
            name: "search".into(),
            execution_environment: "sandbox".into(),
            workspace_access: sylvander_api::McpWorkspaceAccess::Read,
            command: "/opt/bin/search-mcp".into(),
            args: vec!["--token".into(), "also-secret".into()],
            envs: std::collections::HashMap::from([("SEARCH_TOKEN".into(), "super-secret".into())]),
        })
        .ui_command(crate::agent_definition::UiCommandConfig {
            id: "security-review".into(),
            name: "security-review".into(),
            usage: "/security-review [scope]".into(),
            description: "Review a scope".into(),
            hint: "workspace".into(),
            prompt: "Review {{args}} for security issues.".into(),
        })
        .tool_presentations(vec![crate::agent_definition::ToolPresentationConfig {
            tool_name: "search".into(),
            label: "Search".into(),
            kind: sylvander_api::ToolPresentationKind::Search,
            target_field: Some("query".into()),
        }])
        .build()
        .unwrap();
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .unwrap();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .memory(Arc::new(InMemoryMemoryStore::new()))
        .build()
        .unwrap();

    let snapshot = run.platform_snapshot();
    assert_eq!(snapshot.features.len(), 3);
    assert_eq!(snapshot.commands.len(), 1);
    assert_eq!(snapshot.tool_presentations.len(), 1);
    assert_eq!(snapshot.commands[0].source, "agent configuration");
    assert_eq!(
        snapshot.commands[0].trust,
        sylvander_api::PlatformTrust::Workspace
    );
    assert_eq!(
        snapshot.features[0].status,
        sylvander_api::PlatformFeatureStatus::Configured
    );
    assert_eq!(
        snapshot.features[1].kind,
        sylvander_api::PlatformFeatureKind::Memory
    );
    assert_eq!(snapshot.features[1].name, "runtime memory");
    assert_eq!(
        snapshot.features[1].status,
        sylvander_api::PlatformFeatureStatus::Active
    );
    assert_eq!(
        snapshot.features[1].source.as_deref(),
        Some("runtime injection")
    );
    assert_eq!(
        snapshot.features[2].kind,
        sylvander_api::PlatformFeatureKind::Extension
    );
    let json = serde_json::to_string(&snapshot).unwrap();
    assert!(!json.contains("super-secret"));
    assert!(!json.contains("also-secret"));
    assert!(!json.contains("/opt/bin"));
}

#[test]
fn platform_snapshot_reports_runtime_override_without_activating_declarations() {
    let spec = AgentSpec::builder()
        .id("test-agent")
        .name("Test")
        .model_name("test-model")
        .memory_store(crate::agent_definition::MemoryStoreConfig {
            store_type: "sqlite".into(),
            path: PathBuf::from("/private/sentinel-memory.db"),
        })
        .build()
        .unwrap();
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .unwrap();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .memory(Arc::new(InMemoryMemoryStore::new()))
        .build()
        .unwrap();

    let snapshot = run.platform_snapshot();
    let memory = snapshot
        .features
        .iter()
        .filter(|feature| feature.kind == sylvander_api::PlatformFeatureKind::Memory)
        .collect::<Vec<_>>();
    assert_eq!(memory.len(), 2);
    assert_eq!(
        memory
            .iter()
            .filter(|feature| { feature.status == sylvander_api::PlatformFeatureStatus::Active })
            .count(),
        1
    );
    assert_eq!(memory[0].name, "runtime memory");
    assert_eq!(memory[1].name, "sqlite");
    assert_eq!(
        memory[1].status,
        sylvander_api::PlatformFeatureStatus::Configured
    );
    assert_eq!(memory[1].source.as_deref(), Some("agent configuration"));
    assert!(memory[1].capabilities.is_empty());
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("sentinel-memory")
    );
}

#[test]
fn agent_memory_declarations_are_not_implicit_runtime_fallbacks() {
    let spec = AgentSpec::builder()
        .id("test-agent")
        .name("Test")
        .model_name("test-model")
        .memory_store(crate::agent_definition::MemoryStoreConfig {
            store_type: "unsupported-future-store".into(),
            path: PathBuf::from("/private/never-open-this-store"),
        })
        .build()
        .unwrap();
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .unwrap();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build()
        .unwrap();

    assert!(run.inner.memory.is_none());
    let snapshot = run.platform_snapshot();
    let memory = snapshot
        .features
        .iter()
        .filter(|feature| feature.kind == sylvander_api::PlatformFeatureKind::Memory)
        .collect::<Vec<_>>();
    assert_eq!(memory.len(), 1);
    assert_eq!(
        memory[0].status,
        sylvander_api::PlatformFeatureStatus::Configured
    );
    assert_eq!(memory[0].summary, "declared; not activated by runtime");
    assert!(memory[0].capabilities.is_empty());
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("never-open-this-store")
    );
}

#[tokio::test(start_paused = true)]
async fn approval_timeout_rejects_and_clears_the_pending_request() {
    let bus = Arc::new(InProcessMessageBus::new());
    let mut events = bus.subscribe(SubscriptionFilter::all()).await.unwrap();
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let gate = Arc::new(BusApprovalGate {
        bus,
        agent_id: AgentId::new("agent"),
        session_id: SessionId::new("session"),
        grant_context: ApprovalGrantContext::new(
            "user",
            AgentId::new("agent"),
            format!("sha256:{}", "1".repeat(64)),
            format!("sha256:{}", "2".repeat(64)),
        ),
        persistent_identity_authorized: true,
        pending_approvals: pending.clone(),
        approval_memory: Arc::new(Mutex::new(ApprovalMemory::load(None).unwrap())),
    });
    let request = ToolUseRequest {
        call_id: "tool-1".into(),
        tool_name: "write".into(),
        input: serde_json::json!({"path": "notes.md"}),
        facts: ToolApprovalFacts::new(
            ToolInvocationClass::FilesystemMutation,
            ToolExecutionMode::Exclusive,
            ToolExecutionPolicy::workspace_write(),
        ),
    };
    let task = tokio::spawn(async move { gate.check_batch(&[request]).await });

    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::ToolApprovalRequired { .. }
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(121)).await;
    let result = task.await.unwrap();

    assert!(matches!(
        result.decisions.as_slice(),
        [ApprovalDecision::Rejected { reason }] if reason == "approval timeout"
    ));
    assert!(pending.lock().await.is_empty());
    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::InteractionTimedOut {
            kind: sylvander_api::InteractionTimeoutKind::Approval,
            subject_id,
            timeout_secs: 120,
            recovery: sylvander_api::TimeoutRecovery::RetryRequest,
        } if subject_id == "tool-1"
    ));
}

#[tokio::test(start_paused = true)]
async fn question_timeout_returns_empty_and_clears_the_pending_answer() {
    let bus = Arc::new(InProcessMessageBus::new());
    let mut events = bus.subscribe(SubscriptionFilter::all()).await.unwrap();
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let gate = Arc::new(BusAskUserGate {
        bus,
        agent_id: AgentId::new("agent"),
        session_id: SessionId::new("session"),
        pending_answers: pending.clone(),
    });
    let task =
        tokio::spawn(async move { gate.ask("question-1", "Continue?", vec![], false).await });

    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::AskUser { .. }
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(301)).await;

    assert!(task.await.unwrap().is_empty());
    assert!(pending.lock().await.is_empty());
    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::InteractionTimedOut {
            kind: sylvander_api::InteractionTimeoutKind::Question,
            subject_id,
            timeout_secs: 300,
            recovery: sylvander_api::TimeoutRecovery::RetryRequest,
        } if subject_id == "question-1"
    ));
}

#[tokio::test(start_paused = true)]
async fn plan_timeout_rejects_and_clears_the_pending_review() {
    let bus = Arc::new(InProcessMessageBus::new());
    let mut events = bus.subscribe(SubscriptionFilter::all()).await.unwrap();
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let gate = Arc::new(BusPlanGate {
        bus,
        agent_id: AgentId::new("agent"),
        session_id: SessionId::new("session"),
        pending_plans: pending.clone(),
    });
    let task = tokio::spawn(async move { gate.review("plan-1", vec!["inspect".into()]).await });

    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::PlanProposed { .. }
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(301)).await;

    assert!(matches!(
        task.await.unwrap(),
        PlanDecision::Rejected { reason } if reason == "plan review timed out"
    ));
    assert!(pending.lock().await.is_empty());
    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::InteractionTimedOut {
            kind: sylvander_api::InteractionTimeoutKind::Plan,
            subject_id,
            timeout_secs: 300,
            recovery: sylvander_api::TimeoutRecovery::RetryRequest,
        } if subject_id == "plan-1"
    ));
}

#[test]
fn configured_pricing_calculates_nano_usd_and_requires_cache_rates() {
    let pricing = sylvander_api::ModelPricing {
        input_usd_micros_per_million: 3_000_000,
        output_usd_micros_per_million: 15_000_000,
        cache_write_usd_micros_per_million: None,
        cache_read_usd_micros_per_million: Some(300_000),
    };
    let mut usage = TokenUsage {
        input_tokens: 1_000,
        output_tokens: 100,
        cache_write_tokens: None,
        cache_read_tokens: Some(10_000),
        ..TokenUsage::default()
    };
    assert_eq!(usage_cost_nano_usd(pricing, &usage), Some(7_500_000));
    usage.cache_write_tokens = Some(1);
    assert_eq!(usage_cost_nano_usd(pricing, &usage), None);
}

#[tokio::test]
async fn agent_run_is_cloneable() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    let run2 = run.clone();
    assert_eq!(run.id(), run2.id());
}

#[tokio::test]
async fn active_turn_snapshot_is_typed_and_session_scoped() {
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build()
        .expect("build");
    let session_id = SessionId::new("active-session");
    let expected = RuntimeTurnSnapshot {
        turn_id: "turn-1".into(),
        state: sylvander_agent::turn::machine::TurnSnapshot {
            sequence: 4,
            iteration: 1,
            phase: sylvander_agent::turn::machine::TurnPhase::CallingModel,
            continuation: None,
        },
    };
    run.inner
        .turn_snapshots
        .write()
        .await
        .insert(session_id.clone(), expected.clone());

    assert_eq!(run.active_turn_snapshot(&session_id).await, Some(expected));
    assert_eq!(
        run.active_turn_snapshot(&SessionId::new("other-session"))
            .await,
        None
    );
}

#[tokio::test]
async fn agent_run_previews_and_rolls_back_journaled_write() {
    let workspace = tempfile::TempDir::new().unwrap();
    let journal = tempfile::TempDir::new().unwrap();
    let file = workspace.path().join("file.txt");
    std::fs::write(&file, "before").unwrap();
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .workspace_journal(journal.path())
        .build()
        .unwrap();
    let session_id = run
        .join_session(SessionMetadata {
            workspace: workspace.path().into(),
            ..test_metadata()
        })
        .await;
    let context = ToolContext::new(
        AgentExecutionContext::restricted_for("user-1", "test-agent", session_id.0.clone())
            .with_trace_id("turn-1"),
    )
    .with_executor(
        Arc::new(LocalExecutor),
        WorkspaceTarget::local(workspace.path(), false),
    )
    .with_capability(Cap::Write)
    .with_workspace_journal(run.inner.workspace_journal.clone().unwrap());
    let tool = sylvander_agent::tools::WriteTool::new();
    let call = ToolRegistry::new()
        .register(tool)
        .prepare(
            "Write",
            serde_json::json!({"file_path":"file.txt","content":"after"}),
        )
        .unwrap();
    tool.handle(&context, &call).await.unwrap();

    let preview = run.preview_workspace_rollback(&session_id).await.unwrap();
    assert_eq!(preview.files, vec!["file.txt"]);
    run.rollback_workspace_latest(&session_id, &preview.turn_id)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(file).unwrap(), "before");
}

#[tokio::test]
async fn runtime_model_selection_is_catalog_backed_and_capability_checked() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let thinking = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("anthropic", "thinking-model"),
        context_window: 200_000,
        max_output_tokens: 32_000,
        capabilities: sylvander_llm_core::ModelCapabilities::REASONING,
    };
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .available_provider_models(vec![thinking])
        .qualified_model_lifecycles(HashMap::from([(
            sylvander_api::ModelSelection {
                provider_id: "anthropic".into(),
                model_id: "thinking-model".into(),
            },
            sylvander_api::ModelLifecycle::Deprecated {
                replacement: Some("claude-sonnet-5-20260601".into()),
            },
        )]))
        .build()
        .expect("build");

    let initial = run.runtime_model_info().await;
    assert_eq!(initial.current_model, "claude-sonnet-5-20260601");
    assert_eq!(initial.models.len(), 2);
    assert!(matches!(
        initial
            .models
            .iter()
            .find(|model| model.id == "thinking-model")
            .map(|model| &model.lifecycle),
        Some(sylvander_api::ModelLifecycle::Deprecated {
            replacement: Some(replacement)
        }) if replacement == "claude-sonnet-5-20260601"
    ));
    let selected = run
        .select_qualified_model(
            sylvander_api::ModelSelection {
                provider_id: "anthropic".into(),
                model_id: "thinking-model".into(),
            },
            sylvander_api::ReasoningEffort::High,
        )
        .await
        .expect("select");
    assert_eq!(selected.current_model, "thinking-model");
    assert_eq!(
        selected.reasoning_effort,
        sylvander_api::ReasoningEffort::High
    );
    assert!(
        run.select_qualified_model(
            sylvander_api::ModelSelection {
                provider_id: "anthropic".into(),
                model_id: "claude-sonnet-5-20260601".into(),
            },
            sylvander_api::ReasoningEffort::Low,
        )
        .await
        .is_err()
    );
    assert_eq!(
        run.runtime_model_info().await.current_model,
        "thinking-model"
    );
}

#[tokio::test]
async fn context_report_separates_window_usage_from_cumulative_accounting() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    let session_id = run.join_session(test_metadata()).await;
    run.inner
        .sessions
        .write()
        .await
        .get_mut(&session_id)
        .expect("session")
        .append_user_message(ChatMessage::user("hello"));
    run.inner.context_usage.write().await.insert(
        session_id.clone(),
        ContextUsage {
            used: 1_250,
            cache_read: 900,
            cache_write: 120,
        },
    );

    let report = run.context_report(Some(&session_id)).await;
    assert_eq!(report.used_tokens, 1_250);
    assert_eq!(report.cache_read_tokens, 900);
    assert_eq!(report.cache_write_tokens, 120);
    assert_eq!(
        report.remaining_tokens,
        report.context_window.saturating_sub(1_250)
    );
    assert!(report.sources.iter().any(|source| {
        source.kind == sylvander_api::ContextSourceKind::Conversation && source.items == 1
    }));
}

#[tokio::test]
async fn runtime_permissions_are_validated_against_operator_capabilities() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    assert_eq!(
        run.permission_profile().await,
        sylvander_api::PermissionProfile::default()
    );
    let restricted = sylvander_api::PermissionProfile {
        file_access: sylvander_api::FileAccess::ReadOnly,
        network_access: sylvander_api::NetworkAccess::Denied,
        approval_policy: sylvander_api::ApprovalPolicy::Deny,
    };
    assert_eq!(
        run.select_permissions(restricted.clone()).await.unwrap(),
        restricted
    );
    assert!(
        run.select_permissions(sylvander_api::PermissionProfile {
            approval_policy: sylvander_api::ApprovalPolicy::Ask,
            ..Default::default()
        })
        .await
        .is_err()
    );
}

#[test]
fn permission_profile_builds_a_workspace_scoped_tool_context() {
    let metadata = test_metadata();
    let execution_service = crate::execution::RuntimeExecutionService::standalone_local();
    let context = tool_context_for_permissions(
        ToolSessionExecution {
            metadata: &metadata,
            effective_config: None,
            execution_service: &execution_service,
        },
        &AgentId::new("agent"),
        &SessionId::new("session"),
        &sylvander_api::PermissionProfile {
            file_access: sylvander_api::FileAccess::ReadOnly,
            network_access: sylvander_api::NetworkAccess::Allowed,
            approval_policy: sylvander_api::ApprovalPolicy::Deny,
        },
        true,
        None,
        Some("turn-1"),
    );
    assert_eq!(
        context.surface.fs_root.as_deref(),
        Some(metadata.workspace.as_path())
    );
    assert!(context.has_cap(Cap::Read));
    assert!(context.has_cap(Cap::Git));
    assert!(!context.has_cap(Cap::Write));
    assert!(context.has_cap(Cap::Network));
    assert!(context.host_allowed("example.com"));
    assert!(context.has_cap(Cap::MemoryRead));
    assert_eq!(context.user_id(), metadata.user_id);
    assert_eq!(context.trace_id(), Some("turn-1"));
}

#[test]
fn builder_uses_one_immutable_runtime_execution_service() {
    let (spec, client) = test_spec_and_client();
    let remote: Arc<dyn WorkspaceExecutor> = Arc::new(MarkerWorkspaceExecutor::new(b"remote"));
    let execution_service = crate::execution::RuntimeExecutionService::new_for_test([
        (
            "local".to_owned(),
            Arc::new(crate::execution::LocalExecutor) as Arc<dyn WorkspaceExecutor>,
        ),
        ("ssh:build".to_owned(), remote.clone()),
    ])
    .unwrap();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .execution_service(execution_service)
        .build()
        .expect("build");

    assert!(run.inner.execution_service.resolve("local").is_some());
    assert!(Arc::ptr_eq(
        run.inner.execution_service.resolve("ssh:build").unwrap(),
        &remote
    ));
    assert!(run.inner.execution_service.resolve("unknown").is_none());
}

#[tokio::test]
async fn turn_context_resolves_the_effective_execution_target() {
    let metadata = test_metadata();
    let effective = remote_effective_config("ssh:build", "/remote/project");
    let remote = Arc::new(MarkerWorkspaceExecutor::new(b"remote"));
    let execution_service = crate::execution::RuntimeExecutionService::new_for_test([(
        "ssh:build".to_owned(),
        remote.clone() as Arc<dyn WorkspaceExecutor>,
    )])
    .unwrap();
    let context = tool_context_for_permissions(
        ToolSessionExecution {
            metadata: &metadata,
            effective_config: Some(&effective),
            execution_service: &execution_service,
        },
        &AgentId::new("agent"),
        &SessionId::new("session"),
        &sylvander_api::PermissionProfile::default(),
        false,
        None,
        Some("turn-1"),
    );

    let bytes = context
        .executor
        .read_file(&context.execution_target, "README.md")
        .await
        .unwrap();
    assert_eq!(bytes, b"remote");
    assert_eq!(context.execution_target.id, "ssh:build");
    assert_eq!(
        context.execution_target.workspace_path,
        Path::new("/remote/project")
    );
    assert_eq!(
        remote.reads.lock().unwrap().as_slice(),
        &[context.execution_target]
    );
}

#[tokio::test]
async fn executor_resolution_is_rebuilt_after_agent_restart() {
    let metadata = test_metadata();
    let effective = remote_effective_config("container:dev", "/workspace");
    let old: Arc<dyn WorkspaceExecutor> = Arc::new(MarkerWorkspaceExecutor::new(b"old"));
    let new: Arc<dyn WorkspaceExecutor> = Arc::new(MarkerWorkspaceExecutor::new(b"new"));
    let (spec, client) = test_spec_and_client();
    let before_restart = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .execution_service(
            crate::execution::RuntimeExecutionService::new_for_test([(
                "container:dev".into(),
                old,
            )])
            .unwrap(),
        )
        .build()
        .unwrap();
    drop(before_restart);
    let (spec, client) = test_spec_and_client();
    let after_restart = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .execution_service(
            crate::execution::RuntimeExecutionService::new_for_test([(
                "container:dev".into(),
                new,
            )])
            .unwrap(),
        )
        .build()
        .unwrap();
    let permissions = sylvander_api::PermissionProfile::default();
    let context_after_restart = tool_context_for_permissions(
        ToolSessionExecution {
            metadata: &metadata,
            effective_config: Some(&effective),
            execution_service: &after_restart.inner.execution_service,
        },
        &AgentId::new("agent"),
        &SessionId::new("restored-session"),
        &permissions,
        false,
        None,
        Some("new-turn"),
    );

    let bytes = context_after_restart
        .executor
        .read_file(&context_after_restart.execution_target, "Cargo.toml")
        .await
        .unwrap();
    assert_eq!(bytes, b"new");
}

#[tokio::test]
async fn effective_workspace_mounts_route_file_operations_by_logical_reference() {
    let task = tempfile::tempdir().unwrap();
    let dependency = tempfile::tempdir().unwrap();
    std::fs::write(task.path().join("task.txt"), "task").unwrap();
    std::fs::write(dependency.path().join("lib.txt"), "dependency").unwrap();
    let metadata = test_metadata();
    let mut effective = remote_effective_config("local", task.path().to_str().unwrap());
    effective.workspace_mounts = vec![
        sylvander_api::SessionWorkspaceMount {
            reference: "task".into(),
            role: sylvander_api::WorkspaceMountRole::Task,
            binding: sylvander_api::SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: task.path().into(),
                read_only: false,
                instruction_focus: None,
            },
            capabilities: sylvander_api::WorkspaceCapabilityPolicy {
                read: true,
                write: true,
                command: true,
                git: true,
            },
        },
        sylvander_api::SessionWorkspaceMount {
            reference: "shared".into(),
            role: sylvander_api::WorkspaceMountRole::Dependency,
            binding: sylvander_api::SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: dependency.path().into(),
                read_only: true,
                instruction_focus: None,
            },
            capabilities: sylvander_api::WorkspaceCapabilityPolicy {
                read: true,
                git: true,
                ..Default::default()
            },
        },
    ];
    let execution_service = crate::execution::RuntimeExecutionService::new_for_test([(
        "local".into(),
        Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>,
    )])
    .unwrap();
    let context = tool_context_for_permissions(
        ToolSessionExecution {
            metadata: &metadata,
            effective_config: Some(&effective),
            execution_service: &execution_service,
        },
        &AgentId::new("agent"),
        &SessionId::new("session"),
        &sylvander_api::PermissionProfile::default(),
        false,
        None,
        None,
    );

    assert_eq!(
        context
            .executor
            .read_file(&context.execution_target, "task.txt")
            .await
            .unwrap(),
        b"task"
    );
    assert_eq!(
        context
            .executor
            .read_file(&context.execution_target, "@shared/lib.txt")
            .await
            .unwrap(),
        b"dependency"
    );
    assert!(
        context
            .executor
            .write_file(&context.execution_target, "@shared/nope.txt", b"x")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unknown_execution_target_is_explicitly_unavailable() {
    let metadata = test_metadata();
    let effective = remote_effective_config("ssh:missing", "/remote/project");
    let execution_service = crate::execution::RuntimeExecutionService::new_for_test([]).unwrap();
    let context = tool_context_for_permissions(
        ToolSessionExecution {
            metadata: &metadata,
            effective_config: Some(&effective),
            execution_service: &execution_service,
        },
        &AgentId::new("agent"),
        &SessionId::new("session"),
        &sylvander_api::PermissionProfile::default(),
        false,
        None,
        None,
    );

    let error = context
        .executor
        .read_file(&context.execution_target, "README.md")
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        sylvander_agent::workspace_executor::WorkspaceExecutorError::Unavailable(target)
            if target == "ssh:missing"
    ));
}

#[test]
fn user_workspace_precedes_agent_workspace_and_agent_fallback_keeps_read_only() {
    let user = sylvander_api::SessionWorkspaceBinding {
        execution_target: "local".into(),
        path: "/user".into(),
        read_only: false,
        instruction_focus: None,
    };
    let agent = sylvander_api::SessionWorkspaceBinding {
        execution_target: "ssh:agent".into(),
        path: "/agent".into(),
        read_only: true,
        instruction_focus: None,
    };
    assert_eq!(
        select_workspace_binding(Some(&user), Some(&agent)),
        Some(&user)
    );
    let selected = select_workspace_binding(None, Some(&agent)).unwrap();
    assert_eq!(selected.execution_target, "ssh:agent");
    assert!(selected.read_only);
}

#[tokio::test]
async fn interrupt_is_scoped_to_the_selected_session() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    let session_a = SessionId::new("session-a");
    let session_b = SessionId::new("session-b");
    let (interrupt_a, interrupted_a) = oneshot::channel();
    let (interrupt_b, mut interrupted_b) = oneshot::channel();
    run.inner.active_turns.lock().await.insert(
        session_a.clone(),
        ActiveTurn {
            id: uuid::Uuid::new_v4(),
            interrupt: interrupt_a,
        },
    );
    run.inner.active_turns.lock().await.insert(
        session_b,
        ActiveTurn {
            id: uuid::Uuid::new_v4(),
            interrupt: interrupt_b,
        },
    );

    run.inner.interrupt_turn(&session_a).await;

    assert!(interrupted_a.await.is_ok());
    assert!(matches!(
        interrupted_b.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn interactive_decisions_are_scoped_when_ids_collide_across_sessions() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus.clone())
        .build()
        .expect("build");
    let session_a = SessionId::new("session-a");
    let session_b = SessionId::new("session-b");
    let (approval_a_tx, approval_a_rx) = oneshot::channel();
    let (approval_b_tx, mut approval_b_rx) = oneshot::channel();
    let (answer_a_tx, answer_a_rx) = oneshot::channel();
    let (answer_b_tx, mut answer_b_rx) = oneshot::channel();
    let (plan_a_tx, plan_a_rx) = oneshot::channel();
    let (plan_b_tx, mut plan_b_rx) = oneshot::channel();

    for (session, approval, answer, plan) in [
        (&session_a, approval_a_tx, answer_a_tx, plan_a_tx),
        (&session_b, approval_b_tx, answer_b_tx, plan_b_tx),
    ] {
        let grant = ApprovalGrantContext::new(
            "user",
            AgentId::new("agent"),
            format!("sha256:{}", "1".repeat(64)),
            format!("sha256:{}", "2".repeat(64)),
        )
        .key_for(&ToolUseRequest {
            call_id: "shared-id".into(),
            tool_name: "write".into(),
            input: serde_json::json!({"path": "shared"}),
            facts: ToolApprovalFacts::new(
                ToolInvocationClass::FilesystemMutation,
                ToolExecutionMode::Exclusive,
                ToolExecutionPolicy::workspace_write(),
            ),
        });
        run.inner.pending_approvals.lock().await.insert(
            (session.clone(), "shared-id".into()),
            PendingApproval {
                session_id: session.clone(),
                grant,
                persistent_identity_authorized: true,
                allowed_scopes: vec![sylvander_api::ApprovalScope::Once],
                sender: approval,
            },
        );
        run.inner.pending_answers.lock().await.insert(
            (session.clone(), "shared-id".into()),
            PendingAnswer {
                session_id: session.clone(),
                sender: answer,
            },
        );
        run.inner.pending_plans.lock().await.insert(
            (session.clone(), "shared-id".into()),
            PendingPlan {
                session_id: session.clone(),
                sender: plan,
            },
        );
    }

    let inbox = bus.subscribe(run.subscription_filter()).await.unwrap();
    let task = tokio::spawn(run.run(inbox));
    for kind in [
        SystemMessage::ApproveTool {
            call_id: "shared-id".into(),
            approved: false,
            scope: sylvander_api::ApprovalScope::Once,
            reason: Some("session A rejected".into()),
        },
        SystemMessage::AnswerQuestion {
            call_id: "shared-id".into(),
            answer: "session A answer".into(),
        },
        SystemMessage::ResolvePlan {
            plan_id: "shared-id".into(),
            decision: sylvander_api::PlanDecision::Approved,
        },
    ] {
        bus.publish(BusMessage {
            session_id: session_a.clone(),
            sender: sylvander_api::Sender::System,
            recipient: sylvander_api::Recipient::Agent(AgentId::new("test-agent")),
            kind: MessageKind::System(kind),
            payload: String::new(),
            attachments: Vec::new(),
            timestamp: crate::session::now_secs(),
            id: sylvander_api::MessageId::new(),
        })
        .await
        .unwrap();
    }

    assert!(matches!(
        approval_a_rx.await.unwrap(),
        ApprovalDecision::Rejected { reason } if reason == "session A rejected"
    ));
    assert_eq!(answer_a_rx.await.unwrap(), ["session A answer"]);
    assert_eq!(plan_a_rx.await.unwrap(), PlanDecision::Approved);
    assert!(matches!(
        approval_b_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        answer_b_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        plan_b_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    task.abort();
}

async fn failing_persistent_run(
    fail: SessionStoreFailPoint,
    seed_session: bool,
) -> (
    AgentRun,
    AgentSessionIssuer,
    Arc<RecordingProvider>,
    Arc<dyn SessionStore>,
    SessionId,
    SessionMetadata,
) {
    let inner: Arc<dyn SessionStore> = Arc::new(
        crate::storage::session::SqliteSessionStore::open_in_memory()
            .await
            .expect("store"),
    );
    let store: Arc<dyn SessionStore> = Arc::new(FailingSessionStore::new(inner.clone(), fail));
    let (spec, _) = test_spec_and_client();
    let resolver = Arc::new(
        sylvander_agent::prompt::PromptResolver::new(
            "agent:test-agent@1".into(),
            spec.persona.system_prompt.clone(),
            Vec::new(),
            None,
            false,
        )
        .expect("resolver"),
    );
    let provider = Arc::new(RecordingProvider::default());
    let model = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new(
            spec.model.provider.clone(),
            spec.model.model_name.clone(),
        ),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let (run, issuer) = AgentRun::qualified_router_builder(spec, provider.clone(), model)
        .bus(Arc::new(InProcessMessageBus::new()))
        .session_store(store)
        .prompt_resolver(resolver)
        .build_with_session_issuer()
        .expect("run");
    let session_id = SessionId::new("persistence-failure-session");
    let metadata = test_metadata();
    if seed_session {
        let mut stored = StoredSession::new(
            session_id.clone(),
            metadata.name.clone(),
            SessionLifetime::Persistent,
            metadata.clone(),
            vec![run.id().clone()],
        );
        stored.effective_config = Some(run.inner.direct_session_config(&metadata).await);
        inner.save(&stored).await.expect("seed persistent session");
    }
    (run, issuer, provider, inner, session_id, metadata)
}

fn assert_persistence_failure(error: AgentRunError, expected: SessionPersistenceOperation) {
    let rendered = error.to_string();
    assert!(
        matches!(
            error,
            AgentRunError::SessionPersistence { operation, .. } if operation == expected
        ),
        "{rendered}"
    );
    assert!(!rendered.contains("private injected detail"));
}

#[tokio::test]
async fn persistent_session_attach_fails_closed_on_inspect_create_and_restore() {
    for (fail, seed, expected) in [
        (
            SessionStoreFailPoint::Get,
            true,
            SessionPersistenceOperation::InspectSession,
        ),
        (
            SessionStoreFailPoint::Save,
            false,
            SessionPersistenceOperation::CreateSession,
        ),
        (
            SessionStoreFailPoint::ReadHistory,
            true,
            SessionPersistenceOperation::RestoreHistory,
        ),
    ] {
        let (run, issuer, _, _, session_id, metadata) = failing_persistent_run(fail, seed).await;
        let lease = issuer
            .issue(session_id.clone(), metadata)
            .expect("authenticated lease");
        let error = run
            .attach_authenticated_session(lease)
            .await
            .expect_err("persistence failure must reject session attachment");
        assert_persistence_failure(error, expected);
        assert!(!run.list_sessions().await.contains(&session_id));
        assert!(
            !run.inner
                .authenticated_sessions
                .read()
                .await
                .contains(&session_id)
        );
    }
}

#[tokio::test]
async fn persistent_user_write_failure_stops_before_provider_work() {
    let (run, issuer, provider, inner, session_id, metadata) =
        failing_persistent_run(SessionStoreFailPoint::BeginTurn, true).await;
    let authenticated = issuer
        .issue(session_id.clone(), metadata.clone())
        .expect("lease");
    run.attach_authenticated_session(authenticated)
        .await
        .expect("attach");
    let mut events = run
        .inner
        .bus
        .subscribe(SubscriptionFilter::all())
        .await
        .unwrap();

    let error = run
        .handle_message(BusMessage::user_chat(
            session_id.clone(),
            metadata.user_id.clone(),
            "must persist first",
        ))
        .await
        .expect_err("begin-turn failure must terminate the turn");
    assert_persistence_failure(error, SessionPersistenceOperation::BeginTurn);
    let snapshot = run.inner.observability.snapshot();
    assert_eq!(snapshot.turns_started, 1);
    assert_eq!(snapshot.turns_failed, 1);
    assert_eq!(snapshot.persistence_failed, 1);
    assert_eq!(snapshot.active_turns, 0);
    assert_eq!(snapshot.turn_latency.count, 1);
    while let Ok(message) = events.try_recv() {
        assert!(!matches!(
            message.kind,
            MessageKind::Stream(StreamEvent::TurnStarted { .. })
        ));
    }
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!(run.get_session(&session_id).await.unwrap().len(), 0);
    let caller =
        sylvander_api::SessionContext::new(metadata.user_id, run.id().clone(), session_id.clone());
    assert!(
        inner
            .read_history(&caller, &session_id, false, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn persistent_terminal_write_failures_never_publish_done() {
    for (fail, expected) in [
        (
            SessionStoreFailPoint::RecordUsage,
            SessionPersistenceOperation::RecordUsage,
        ),
        (
            SessionStoreFailPoint::AppendMessage,
            SessionPersistenceOperation::CompleteTurn,
        ),
    ] {
        let (run, issuer, provider, inner, session_id, metadata) =
            failing_persistent_run(fail, true).await;
        let mut events = run
            .inner
            .bus
            .subscribe(SubscriptionFilter::all())
            .await
            .unwrap();
        let authenticated = issuer
            .issue(session_id.clone(), metadata.clone())
            .expect("lease");
        run.attach_authenticated_session(authenticated)
            .await
            .expect("attach");

        let error = run
            .handle_message(BusMessage::user_chat(
                session_id.clone(),
                metadata.user_id.clone(),
                "produce a durable answer",
            ))
            .await
            .expect_err("terminal persistence failure must fail the turn");
        assert_persistence_failure(error, expected);
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
        assert_eq!(run.get_session(&session_id).await.unwrap().len(), 1);
        let caller = sylvander_api::SessionContext::new(
            metadata.user_id,
            run.id().clone(),
            session_id.clone(),
        );
        let history = inner
            .read_history(&caller, &session_id, false, None)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, StoredMessageRole::User);

        let mut saw_error = false;
        while let Ok(message) = events.try_recv() {
            if let MessageKind::Stream(event) = message.kind {
                assert!(!matches!(event, StreamEvent::Done { .. }));
                if let StreamEvent::Error { message } = event {
                    saw_error = true;
                    assert!(!message.contains("private injected detail"));
                }
            }
        }
        assert!(saw_error);
    }
}

#[tokio::test]
async fn compacted_history_write_failure_keeps_live_and_durable_history_unchanged() {
    let (run, issuer, _, inner, session_id, metadata) =
        failing_persistent_run(SessionStoreFailPoint::ReplaceHistory, true).await;
    let authenticated = issuer
        .issue(session_id.clone(), metadata.clone())
        .expect("lease");
    run.attach_authenticated_session(authenticated)
        .await
        .expect("attach");
    let replacement = vec![ChatMessage::user(
        "[Earlier conversation summary]\nimportant decision",
    )];
    let layers = vec![sylvander_agent::compress::layer::LayerReport {
        name: "auto_compact".into(),
        removed_count: 2,
        freed_tokens: 100,
        details: Some(serde_json::json!({"summary": "important decision"})),
        ..Default::default()
    }];

    let error = run
        .inner
        .apply_compacted_history(&session_id, &replacement, &layers)
        .await
        .expect_err("history replacement must fail closed");
    assert_persistence_failure(error, SessionPersistenceOperation::ReplaceHistory);
    assert_eq!(run.get_session(&session_id).await.unwrap().len(), 0);
    let caller =
        sylvander_api::SessionContext::new(metadata.user_id, run.id().clone(), session_id.clone());
    assert!(
        inner
            .read_history(&caller, &session_id, false, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn durable_session_history_restores_into_agent_context() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let agent_id = spec.id.clone();
    let store: Arc<dyn SessionStore> = Arc::new(
        crate::storage::session::SqliteSessionStore::open_in_memory()
            .await
            .expect("store"),
    );
    let session_id = SessionId::new("durable-session");
    let metadata = test_metadata();
    store
        .save(&StoredSession::new(
            session_id.clone(),
            metadata.name.clone(),
            SessionLifetime::Persistent,
            metadata.clone(),
            vec![agent_id.clone()],
        ))
        .await
        .expect("save session");
    let caller =
        sylvander_api::SessionContext::new(metadata.user_id.clone(), agent_id, session_id.clone());
    store
        .append_message(
            &caller,
            &session_id,
            StoredMessageRole::User,
            serde_json::to_value(ChatMessage::user("remember me")).expect("serialize"),
            None,
            None,
            None,
        )
        .await
        .expect("append");

    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .session_store(store)
        .build()
        .expect("build");
    let restored = run
        .inner
        .restore_session_context(&session_id, &metadata)
        .await
        .expect("restore durable history");

    assert_eq!(restored.len(), 1);
}

#[tokio::test]
async fn direct_join_persists_an_auditable_effective_configuration() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let resolver = Arc::new(
        sylvander_agent::prompt::PromptResolver::new(
            "agent:test-agent@1".into(),
            spec.persona.system_prompt.clone(),
            Vec::new(),
            None,
            false,
        )
        .expect("resolver"),
    );
    let store: Arc<dyn SessionStore> = Arc::new(
        crate::storage::session::SqliteSessionStore::open_in_memory()
            .await
            .expect("store"),
    );
    let session_id = SessionId::new("direct-session");
    let metadata = test_metadata();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .session_store(store.clone())
        .prompt_resolver(resolver)
        .build()
        .expect("build");

    run.inner
        .restore_session_context(&session_id, &metadata)
        .await
        .expect("persist direct session");

    let stored = store.get(&session_id).await.unwrap().unwrap();
    let effective = stored
        .effective_config
        .expect("direct session must snapshot runtime defaults");
    assert_eq!(effective.agent_id, run.id().clone());
    assert!(!effective.prompt_manifest.layers.is_empty());
    assert_eq!(effective.user_workspace.unwrap().path, metadata.workspace);
    assert_eq!(
        effective.provenance.model.kind,
        sylvander_api::SessionConfigSourceKind::AgentDefault
    );
}

#[tokio::test]
async fn compacted_history_replaces_runtime_and_durable_active_history() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let agent_id = spec.id.clone();
    let store: Arc<dyn SessionStore> = Arc::new(
        crate::storage::session::SqliteSessionStore::open_in_memory()
            .await
            .expect("store"),
    );
    let session_id = SessionId::new("compact-session");
    let metadata = test_metadata();
    store
        .save(&StoredSession::new(
            session_id.clone(),
            metadata.name.clone(),
            SessionLifetime::Persistent,
            metadata.clone(),
            vec![agent_id.clone()],
        ))
        .await
        .expect("save");
    let caller =
        sylvander_api::SessionContext::new(metadata.user_id.clone(), agent_id, session_id.clone());
    for index in 0..6 {
        store
            .append_message(
                &caller,
                &session_id,
                StoredMessageRole::User,
                serde_json::to_value(ChatMessage::user(format!("message {index}")))
                    .expect("serialize"),
                None,
                None,
                None,
            )
            .await
            .expect("append");
    }
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .session_store(store.clone())
        .build()
        .expect("build");
    run.inner.sessions.write().await.insert(
        session_id.clone(),
        SessionContext::new(session_id.clone(), metadata),
    );
    let history = vec![
        ChatMessage::user("[Earlier conversation summary]\nimportant decisions"),
        ChatMessage::user("recent one"),
        ChatMessage::user("recent two"),
    ];
    let layers = vec![sylvander_agent::compress::layer::LayerReport {
        name: "auto_compact".into(),
        removed_count: 4,
        freed_tokens: 500,
        details: Some(serde_json::json!({"summary": "important decisions"})),
        ..Default::default()
    }];
    run.inner
        .apply_compacted_history(&session_id, &history, &layers)
        .await
        .expect("replace history");

    assert_eq!(
        run.get_session(&session_id).await.expect("session").len(),
        3
    );
    let active = store
        .read_history(&caller, &session_id, false, None)
        .await
        .expect("active history");
    assert_eq!(active.len(), 3);
    assert!(
        active[0]
            .content
            .to_string()
            .contains("important decisions")
    );
}

#[tokio::test]
async fn memory_is_infrastructure_not_tool() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let store = Arc::new(InMemoryMemoryStore::new());
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .memory(store)
        .user_profile_provider(Arc::new(FixedUserProfile(profile_with_learning(false))))
        .build()
        .expect("build");
    let tools = run.memory_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].spec().name, "read_memory");
}

#[tokio::test]
async fn session_capability_is_bound_to_one_run() {
    let (spec_a, client_a) = test_spec_and_client();
    let (run_a, issuer_a) = qualified_anthropic_run_builder(spec_a, client_a)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build_with_session_issuer()
        .expect("build A");
    let (spec_b, client_b) = test_spec_and_client();
    let (run_b, _) = qualified_anthropic_run_builder(spec_b, client_b)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build_with_session_issuer()
        .expect("build B");
    let session_id = SessionId::new("session-a");
    let lease = issuer_a
        .issue(session_id, test_metadata())
        .expect("issue lease");

    let error = run_b
        .attach_authenticated_session(lease)
        .await
        .expect_err("foreign run must reject lease");
    assert!(matches!(error, AgentRunError::Authentication(_)));
    assert!(run_a.list_sessions().await.is_empty());
    assert!(run_b.list_sessions().await.is_empty());
}

#[test]
fn session_issuer_rejects_control_characters_before_admission() {
    let (spec, client) = test_spec_and_client();
    let (_, issuer) = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build_with_session_issuer()
        .expect("build");
    let error = issuer
        .issue(
            SessionId::new("sentinel-session"),
            SessionMetadata {
                user_id: "victim\nforged".into(),
                ..test_metadata()
            },
        )
        .err()
        .expect("unsafe identity must fail");
    assert!(matches!(error, AgentRunError::Authentication(_)));
}

#[tokio::test]
async fn raw_session_presence_has_no_trusted_memory_identity() {
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .memory(Arc::new(InMemoryMemoryStore::new()))
        .build()
        .expect("build");
    let session_id = SessionId::new("raw-bus-session");
    run.inner.sessions.write().await.insert(
        session_id.clone(),
        SessionContext::new(session_id.clone(), test_metadata()),
    );

    assert!(matches!(
        run.memory_context_for_session(&session_id).await,
        Err(MemoryStoreError::AccessDenied)
    ));
}

#[tokio::test]
async fn remember_is_system_driven() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let store = Arc::new(InMemoryMemoryStore::new());
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .memory(store)
        .user_profile_provider(Arc::new(FixedUserProfile(profile_with_learning(false))))
        .build()
        .expect("build");
    let session_id = run.join_session(test_metadata()).await;
    let session = run.authenticated_session_for_test(session_id);
    run.remember(&session, "User prefers dark mode", &["preference"])
        .await
        .expect("remember");
    let results = run
        .recall(
            &session,
            "dark mode",
            sylvander_agent::memory::store::MemoryFilter::default(),
        )
        .await
        .expect("search");
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn remember_derives_identity_from_attached_session() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let store = Arc::new(InMemoryMemoryStore::new());
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .memory(store)
        .user_profile_provider(Arc::new(FixedUserProfile(profile_with_learning(false))))
        .build()
        .expect("build");
    let session_id = run
        .join_session(SessionMetadata {
            user_id: "actual-user".into(),
            ..test_metadata()
        })
        .await;
    let session = run.authenticated_session_for_test(session_id);
    let entry = run.remember(&session, "caller-owned", &[]).await.unwrap();

    assert_eq!(
        entry.owner,
        sylvander_agent::memory::store::MemoryOwner::Relationship {
            user_id: KernelUserId::new("actual-user"),
            agent_id: KernelAgentId::new(run.id().0.clone()),
        }
    );
    assert_eq!(
        run.recall(
            &session,
            "caller-owned",
            sylvander_agent::memory::store::MemoryFilter::default(),
        )
        .await
        .unwrap()
        .len(),
        1
    );
}

#[tokio::test]
async fn remember_denies_opt_out_missing_and_unavailable_profile_authority() {
    let providers = [
        Some(Arc::new(FixedUserProfile(profile_with_learning(true)))
            as Arc<
                dyn sylvander_agent::user_profile_provider::UserProfileProvider,
            >),
        Some(Arc::new(UnavailableUserProfile)
            as Arc<
                dyn sylvander_agent::user_profile_provider::UserProfileProvider,
            >),
        None,
    ];
    for provider in providers {
        let (spec, client) = test_spec_and_client();
        let mut builder = qualified_anthropic_run_builder(spec, client)
            .bus(Arc::new(InProcessMessageBus::new()))
            .memory(Arc::new(InMemoryMemoryStore::new()));
        if let Some(provider) = provider {
            builder = builder.user_profile_provider(provider);
        }
        let run = builder.build().unwrap();
        let session_id = run.join_session(test_metadata()).await;
        let session = run.authenticated_session_for_test(session_id);

        assert!(matches!(
            run.remember(&session, "must not persist", &[]).await,
            Err(MemoryStoreError::AccessDenied)
        ));
        assert!(
            run.recall(
                &session,
                "must not persist",
                sylvander_agent::memory::store::MemoryFilter::default(),
            )
            .await
            .unwrap()
            .is_empty()
        );
    }
}

#[tokio::test]
async fn remember_fails_without_memory_configured() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    let session_id = run.join_session(test_metadata()).await;
    let session = run.authenticated_session_for_test(session_id);
    let err = run.remember(&session, "something", &[]).await.unwrap_err();
    assert!(err.to_string().contains("no memory store"));
}

#[tokio::test]
async fn memory_tools_empty_without_memory_configured() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    assert!(run.memory_tools().is_empty());
}

#[test]
fn typed_attachments_become_provider_content_blocks() {
    let message = BusMessage::user_chat_with_attachments(
        SessionId::new("s1"),
        "u1",
        "review this",
        vec![sylvander_api::MessageAttachment {
            id: "a1".into(),
            kind: sylvander_api::AttachmentKind::File,
            name: "src/main.rs".into(),
            mime_type: "text/x-rust".into(),
            content: sylvander_api::AttachmentContent::Text {
                text: "fn main() {}".into(),
            },
            byte_count: 12,
        }],
    );
    let value = serde_json::to_value(AgentRunInner::message_to_param(&message)).expect("json");
    let content = value["content"].as_array().expect("content blocks");
    assert_eq!(content.len(), 2);
    assert!(content[1]["text"].as_str().unwrap().contains("src/main.rs"));
}

#[tokio::test]
async fn join_and_leave_session() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    let sid = run.join_session(test_metadata()).await;
    assert_eq!(run.list_sessions().await.len(), 1);
    run.leave_session(&sid).await;
    assert!(run.list_sessions().await.is_empty());
}

#[tokio::test]
async fn subscription_filter_matches_agent_and_broadcast() {
    let bus = Arc::new(InProcessMessageBus::new());
    let spec = AgentSpec::builder()
        .id("filter-test")
        .name("Filter Test")
        .model_name("claude-sonnet-5-20260601")
        .build()
        .expect("spec");
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .expect("client");
    let run = qualified_anthropic_run_builder(spec, client)
        .bus(bus.clone())
        .build()
        .expect("build");
    let filter = run.subscription_filter();
    let agent_id = AgentId::new("filter-test");
    assert!(filter.matches(&BusMessage {
        recipient: Recipient::Agent(agent_id.clone()),
        ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi")
    }));
    assert!(filter.matches(&BusMessage {
        recipient: Recipient::Broadcast,
        ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi")
    }));
    assert!(!filter.matches(&BusMessage {
        recipient: Recipient::Agent(AgentId::new("other")),
        ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi")
    }));
}
