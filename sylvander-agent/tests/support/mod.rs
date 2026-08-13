#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::Stream;
use serde_json::Value as JsonValue;
use sylvander_agent::approval::ApprovalGate;
use sylvander_agent::context::compression::pipeline::CompressionPipeline;
use sylvander_agent::execution::artifact::{
    ArtifactReference, ArtifactStoreError, ArtifactWrite, TurnArtifactStore,
};
use sylvander_agent::execution::ports::AgentExecutionPorts;
use sylvander_agent::execution::tool_context::ToolContext;
use sylvander_agent::execution::workspace::{
    WorkspaceCommandOutput, WorkspaceExecutor, WorkspaceExecutorError, WorkspaceListRequest,
    WorkspaceListResult, WorkspaceSearchRequest, WorkspaceSearchResult, WorkspaceTarget,
};
use sylvander_agent::interaction::ask_user::AskUserGate;
use sylvander_agent::interaction::background_task::TaskGate;
use sylvander_agent::interaction::plan::PlanGate;
use sylvander_agent::prelude::{AgentEvent, AgentLoop};
use sylvander_agent::tool::invocation::{
    RegistryBoundToolGateway, ToolInvocationGateway, ToolInvocationSnapshot,
};
use sylvander_agent::tool::{
    PreparedToolCall, RegisteredTool, ToolDefinition, ToolError, ToolExecutor, ToolOutput,
    ToolRegistry, ToolSpec,
};
use sylvander_agent::turn::conversation::ConversationSnapshot;
use sylvander_agent::turn::error::AgentLoopError;
use sylvander_agent::turn::execution_context::AgentExecutionContext;
use sylvander_agent::turn::outcome::AgentOutcome;
use sylvander_agent::turn::request::AgentTurnRequest;
use sylvander_llm_anthropic::{
    AnthropicProvider,
    api::{
        client::AnthropicClient,
        model::{ModelCapabilities as AnthropicModelCapabilities, ModelInfo as AnthropicModelInfo},
    },
};
use sylvander_llm_core::{
    CacheHint, ChatMessage, InputSchema, ModelCapabilities as ProviderModelCapabilities,
    ModelInfo as ProviderModelInfo, ModelProvider, ModelRef, SystemInstruction,
};

pub(crate) struct TestAgentBuilder {
    provider: Arc<dyn ModelProvider>,
    model: ProviderModelInfo,
    kernel: sylvander_agent::kernel::agent_loop::AgentLoopBuilder,
    tools: ToolRegistry,
    tool_context: ToolContext,
    system_prompt: Option<String>,
    approval_gate: Option<Arc<dyn ApprovalGate>>,
    ask_user_gate: Option<Arc<dyn AskUserGate>>,
    plan_gate: Option<Arc<dyn PlanGate>>,
    task_gate: Option<Arc<dyn TaskGate>>,
    artifact_store: Option<Arc<dyn TurnArtifactStore>>,
    invocation_gateway: Option<Arc<dyn ToolInvocationGateway>>,
}

pub(crate) struct TestAgent {
    kernel: AgentLoop,
    provider: Arc<dyn ModelProvider>,
    model: ProviderModelInfo,
    tools: ToolRegistry,
    tool_context: ToolContext,
    system_prompt: Option<String>,
    approval_gate: Option<Arc<dyn ApprovalGate>>,
    ask_user_gate: Option<Arc<dyn AskUserGate>>,
    plan_gate: Option<Arc<dyn PlanGate>>,
    task_gate: Option<Arc<dyn TaskGate>>,
    artifact_store: Option<Arc<dyn TurnArtifactStore>>,
    invocation_gateway: Arc<dyn ToolInvocationGateway>,
    invocation_snapshot: ToolInvocationSnapshot,
}

impl TestAgentBuilder {
    pub(crate) fn tool<T: RegisteredTool + 'static>(mut self, tool: T) -> Self {
        self.tools = self.tools.register(tool);
        self
    }

    pub(crate) fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    pub(crate) fn tool_context(mut self, context: ToolContext) -> Self {
        self.tool_context = context;
        self
    }

    pub(crate) fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub(crate) fn compression_pipeline(mut self, pipeline: CompressionPipeline) -> Self {
        self.kernel = self.kernel.compression_pipeline(pipeline);
        self
    }

    pub(crate) fn max_iterations(mut self, value: u32) -> Self {
        self.kernel = self.kernel.max_iterations(value);
        self
    }

    pub(crate) fn max_retries(mut self, value: u32) -> Self {
        self.kernel = self.kernel.max_retries(value);
        self
    }

    pub(crate) fn approval_gate(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.approval_gate = Some(gate);
        self
    }

    pub(crate) fn ask_user_gate(mut self, gate: Arc<dyn AskUserGate>) -> Self {
        self.ask_user_gate = Some(gate);
        self
    }

    pub(crate) fn plan_gate(mut self, gate: Arc<dyn PlanGate>) -> Self {
        self.plan_gate = Some(gate);
        self
    }

    pub(crate) fn task_gate(mut self, gate: Arc<dyn TaskGate>) -> Self {
        self.task_gate = Some(gate);
        self
    }

    pub(crate) fn artifact_store(mut self, store: Arc<dyn TurnArtifactStore>) -> Self {
        self.artifact_store = Some(store);
        self
    }

    pub(crate) fn invocation_gateway(mut self, gateway: Arc<dyn ToolInvocationGateway>) -> Self {
        self.invocation_gateway = Some(gateway);
        self
    }

    pub(crate) fn build(self) -> Result<TestAgent, AgentLoopError> {
        let invocation_gateway = self
            .invocation_gateway
            .unwrap_or_else(|| RegistryBoundToolGateway::new(self.tools.invocation_descriptors()));
        let invocation_snapshot = invocation_gateway.snapshot();
        let agent = TestAgent {
            kernel: self.kernel.build(),
            provider: self.provider,
            model: self.model,
            tools: self.tools,
            tool_context: self.tool_context,
            system_prompt: self.system_prompt,
            approval_gate: self.approval_gate,
            ask_user_gate: self.ask_user_gate,
            plan_gate: self.plan_gate,
            task_gate: self.task_gate,
            artifact_store: self.artifact_store,
            invocation_gateway,
            invocation_snapshot,
        };
        let (request, ports) = agent.turn(Vec::new());
        ports.validate_for(&request)?;
        Ok(agent)
    }
}

impl TestAgent {
    fn turn(&self, messages: Vec<ChatMessage>) -> (AgentTurnRequest, AgentExecutionPorts) {
        let system_instructions = self
            .system_prompt
            .iter()
            .map(|text| SystemInstruction {
                text: text.clone(),
                cache_hint: self
                    .model
                    .capabilities
                    .contains(ProviderModelCapabilities::PROMPT_CACHING)
                    .then_some(CacheHint::Ephemeral),
            })
            .collect();
        let request = AgentTurnRequest {
            conversation: ConversationSnapshot::new(messages),
            model: self.model.clone(),
            system_instructions,
            reasoning: None,
            tools: self.tools.clone(),
            execution: self.tool_context.execution.as_ref().clone(),
        };
        let mut ports = AgentExecutionPorts::new(
            self.provider.clone(),
            self.tool_context.clone(),
            self.invocation_gateway.clone(),
            self.invocation_snapshot.clone(),
        );
        if let Some(gate) = &self.approval_gate {
            ports = ports.with_approval_gate(gate.clone());
        }
        if let Some(gate) = &self.ask_user_gate {
            ports = ports.with_ask_user_gate(gate.clone());
        }
        if let Some(gate) = &self.plan_gate {
            ports = ports.with_plan_gate(gate.clone());
        }
        if let Some(gate) = &self.task_gate {
            ports = ports.with_task_gate(gate.clone());
        }
        if let Some(store) = &self.artifact_store {
            ports = ports.with_artifact_store(store.clone());
        }
        (request, ports)
    }

    pub(crate) async fn run(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Result<AgentOutcome, AgentLoopError> {
        let (request, ports) = self.turn(messages);
        sylvander_agent::kernel::agent_loop::run(&self.kernel, request, ports).await
    }

    pub(crate) async fn run_with_events<F>(
        &self,
        messages: Vec<ChatMessage>,
        on_event: F,
    ) -> Result<AgentOutcome, AgentLoopError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let (request, ports) = self.turn(messages);
        sylvander_agent::kernel::agent_loop::run_with_events(&self.kernel, request, ports, on_event)
            .await
    }

    pub(crate) fn run_stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> impl Stream<Item = AgentEvent> + Send + '_ {
        let (request, ports) = self.turn(messages);
        sylvander_agent::kernel::agent_loop::run_stream(&self.kernel, request, ports)
    }
}

/// Build an Agent loop through the sole current provider-qualified API.
pub(crate) fn qualified_anthropic_loop_builder(
    client: AnthropicClient,
    model: AnthropicModelInfo,
) -> TestAgentBuilder {
    assert!(
        model.cache_ttl.is_empty(),
        "provider-neutral test models cannot carry Anthropic-only cache TTL metadata"
    );

    let mut capabilities = ProviderModelCapabilities::empty();
    for (anthropic, provider) in [
        (
            AnthropicModelCapabilities::EXTENDED_THINKING,
            ProviderModelCapabilities::REASONING,
        ),
        (
            AnthropicModelCapabilities::PROMPT_CACHING,
            ProviderModelCapabilities::PROMPT_CACHING,
        ),
        (
            AnthropicModelCapabilities::STRUCTURED_OUTPUT,
            ProviderModelCapabilities::STRUCTURED_OUTPUT,
        ),
        (
            AnthropicModelCapabilities::TOOL_USE,
            ProviderModelCapabilities::TOOL_USE,
        ),
        (
            AnthropicModelCapabilities::VISION,
            ProviderModelCapabilities::VISION,
        ),
        (
            AnthropicModelCapabilities::DOCUMENT_INPUT,
            ProviderModelCapabilities::DOCUMENT_INPUT,
        ),
    ] {
        if model.capabilities.contains(anthropic) {
            capabilities |= provider;
        }
    }

    let provider_model = ProviderModelInfo {
        reference: ModelRef::new("anthropic", model.id),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        capabilities,
    };

    TestAgentBuilder {
        provider: Arc::new(AnthropicProvider::new("anthropic", client)),
        model: provider_model,
        kernel: AgentLoop::builder(),
        tools: ToolRegistry::new(),
        tool_context: sylvander_agent::execution::tool_context::defaults::system_tool_context(),
        system_prompt: None,
        approval_gate: None,
        ask_user_gate: None,
        plan_gate: None,
        task_gate: None,
        artifact_store: None,
        invocation_gateway: None,
    }
}

/// Build an explicit workspace-bound context for integration tools.
pub(crate) fn workspace_tool_context(
    root: &std::path::Path,
    capabilities: impl IntoIterator<Item = sylvander_agent::execution::tool_context::Cap>,
) -> ToolContext {
    let target = WorkspaceTarget::local(root.to_path_buf(), false);
    capabilities.into_iter().fold(
        ToolContext::new(AgentExecutionContext::restricted_for(
            "test-user",
            "test-agent",
            "test-session",
        ))
        .with_executor(Arc::new(TestWorkspaceExecutor), target),
        ToolContext::with_capability,
    )
}

/// Minimal integration-test adapter proving that Agent uses an injected port.
struct TestWorkspaceExecutor;

impl fmt::Debug for TestWorkspaceExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TestWorkspaceExecutor")
    }
}

#[async_trait]
impl WorkspaceExecutor for TestWorkspaceExecutor {
    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        let root = tokio::fs::canonicalize(&target.workspace_path).await?;
        let path = tokio::fs::canonicalize(root.join(relative_path)).await?;
        if !path.starts_with(&root) {
            return Err(WorkspaceExecutorError::InvalidPath(
                "path escapes integration-test workspace".to_string(),
            ));
        }
        Ok(tokio::fs::read(path).await?)
    }

    async fn write_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        let root = tokio::fs::canonicalize(&target.workspace_path).await?;
        let path = root.join(relative_path);
        let parent = path.parent().ok_or_else(|| {
            WorkspaceExecutorError::InvalidPath("file parent is required".to_string())
        })?;
        tokio::fs::create_dir_all(parent).await?;
        let parent = tokio::fs::canonicalize(parent).await?;
        if !parent.starts_with(&root) {
            return Err(WorkspaceExecutorError::InvalidPath(
                "path escapes integration-test workspace".to_string(),
            ));
        }
        tokio::fs::write(path, content).await?;
        Ok(())
    }

    async fn run_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let mut process = tokio::process::Command::new("sh");
        process
            .arg("-c")
            .arg(command)
            .current_dir(&target.workspace_path);
        let output = tokio::time::timeout(timeout, process.output())
            .await
            .map_err(|_| WorkspaceExecutorError::Timeout(timeout))??;
        Ok(WorkspaceCommandOutput {
            success: output.status.success(),
            status_code: output.status.code(),
            stdout_total_bytes: output.stdout.len() as u64,
            stderr_total_bytes: output.stderr.len() as u64,
            stdout: output.stdout,
            stderr: output.stderr,
            stdout_truncated: false,
            stderr_truncated: false,
        })
    }

    async fn list(
        &self,
        _target: &WorkspaceTarget,
        _request: WorkspaceListRequest,
    ) -> Result<WorkspaceListResult, WorkspaceExecutorError> {
        Err(read_only_test_error())
    }

    async fn search(
        &self,
        _target: &WorkspaceTarget,
        _request: WorkspaceSearchRequest,
    ) -> Result<WorkspaceSearchResult, WorkspaceExecutorError> {
        Err(read_only_test_error())
    }
}

fn read_only_test_error() -> WorkspaceExecutorError {
    WorkspaceExecutorError::InvalidRequest(
        "operation is outside the read-only integration fixture".to_string(),
    )
}

/// In-memory tool double for public-contract integration tests.
#[derive(Debug, Clone)]
pub(crate) struct MockTool {
    name: String,
    description: String,
    schema: InputSchema,
    responses: Vec<ToolOutput>,
    calls: Arc<Mutex<Vec<JsonValue>>>,
}

impl MockTool {
    pub(crate) fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        response: ToolOutput,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema: InputSchema::empty(),
            responses: vec![response],
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn with_schema(mut self, schema: InputSchema) -> Self {
        self.schema = schema;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn with_responses(mut self, responses: Vec<ToolOutput>) -> Self {
        self.responses = responses;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn calls(&self) -> Vec<JsonValue> {
        self.calls.lock().expect("MockTool lock poisoned").clone()
    }

    #[allow(dead_code)]
    pub(crate) fn call_count(&self) -> usize {
        self.calls.lock().expect("MockTool lock poisoned").len()
    }
}

impl ToolDefinition for MockTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::immediate(
            self.name.clone(),
            self.description.clone(),
            self.schema.schema.clone(),
            sylvander_agent::tool::invocation::ToolInvocationClass::Extension,
        )
    }
}

#[async_trait]
impl ToolExecutor for MockTool {
    async fn handle(
        &self,
        _ctx: &ToolContext,
        call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        let index = {
            let mut calls = self.calls.lock().expect("MockTool lock poisoned");
            calls.push(call.input().clone());
            calls.len() - 1
        };
        self.responses
            .get(index)
            .or_else(|| self.responses.last())
            .cloned()
            .ok_or_else(|| ToolError::Other("no responses configured".into()))
    }
}

/// In-memory oversized-result sink for public-contract integration tests.
#[derive(Default, Clone)]
pub(crate) struct InMemoryArtifactStore {
    inner: Arc<Mutex<HashMap<String, String>>>,
    write_count: Arc<Mutex<usize>>,
}

impl InMemoryArtifactStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn get(&self, tool_use_id: &str) -> Option<String> {
        self.inner.lock().unwrap().get(tool_use_id).cloned()
    }

    pub(crate) fn write_count(&self) -> usize {
        *self.write_count.lock().unwrap()
    }

    #[allow(dead_code)]
    pub(crate) fn ids(&self) -> Vec<String> {
        let mut ids = self
            .inner
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }
}

#[async_trait]
impl TurnArtifactStore for InMemoryArtifactStore {
    async fn persist(
        &self,
        artifact: ArtifactWrite,
    ) -> Result<ArtifactReference, ArtifactStoreError> {
        let original_bytes = artifact.payload.len();
        let body =
            String::from_utf8(artifact.payload).map_err(|_| ArtifactStoreError::InvalidRequest)?;
        self.inner
            .lock()
            .unwrap()
            .insert(artifact.call_id.clone(), body);
        *self.write_count.lock().unwrap() += 1;
        Ok(ArtifactReference {
            locator: format!("artifact:{}", artifact.call_id),
            original_bytes,
        })
    }
}
