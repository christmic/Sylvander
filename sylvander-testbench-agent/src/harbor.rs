//! Harbor task-environment composition for the Sylvander Agent kernel.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use sylvander_agent::execution_context::{
    AgentExecutionContext, ExecutionCapability, ExecutionWorkspace,
};
use sylvander_agent::execution_ports::AgentExecutionPorts;
use sylvander_agent::loop_::{AgentLoop, run_stream};
use sylvander_agent::request::AgentTurnRequest;
use sylvander_agent::tool::ToolRegistry;
use sylvander_agent::tool_context::{Cap, ExecutionBudget, ToolContext};
use sylvander_agent::tool_invocation::{RegistryBoundToolGateway, ToolInvocationGateway};
use sylvander_agent::tools::command::CommandTool;
use sylvander_agent::workspace_executor::{
    ProcessIsolation, WorkspaceCommandOutput, WorkspaceExecutor, WorkspaceExecutorError,
    WorkspaceTarget,
};
use sylvander_llm_core::{
    ChatMessage, ModelCapabilities, ModelInfo, ModelProvider, ModelRef, SystemInstruction,
};

use crate::{RecorderError, Trajectory, TrajectoryRecorder};

#[derive(Debug, Clone)]
pub struct HarborRunConfig {
    pub session_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub workspace: std::path::PathBuf,
    pub instruction: String,
    pub max_iterations: u32,
    pub max_output_tokens: u32,
    pub timeout: Duration,
}

pub async fn run_harbor_task(
    provider: Arc<dyn ModelProvider>,
    config: HarborRunConfig,
) -> Result<Trajectory, RecorderError> {
    let model_ref = ModelRef::new(&config.provider_id, &config.model_id);
    let model = ModelInfo {
        reference: model_ref,
        context_window: 200_000,
        max_output_tokens: config.max_output_tokens,
        capabilities: ModelCapabilities::TOOL_USE | ModelCapabilities::REASONING,
    };
    let tools = ToolRegistry::new().register(CommandTool::new());
    let execution = AgentExecutionContext::restricted_for(
        "harbor-user",
        "sylvander-harbor-agent",
        &config.session_id,
    )
    .with_workspace(ExecutionWorkspace {
        workspace_id: "harbor-task".into(),
        target_id: "harbor-container".into(),
        read_only: false,
    })
    .with_capability(ExecutionCapability::Process)
    .with_timeout(config.timeout);
    let target = WorkspaceTarget {
        id: "harbor-container".into(),
        workspace_path: config.workspace,
        read_only: false,
    };
    let tool_context = ToolContext::new(execution.clone())
        .with_executor(Arc::new(HarborContainerExecutor), target)
        .with_capability(Cap::Spawn)
        .with_budget(ExecutionBudget {
            timeout: Some(config.timeout),
            ..ExecutionBudget::default()
        });
    let gateway = RegistryBoundToolGateway::new(tools.invocation_descriptors());
    let request = AgentTurnRequest {
        conversation: sylvander_agent::conversation::ConversationSnapshot::new(vec![
            ChatMessage::user(&config.instruction),
        ]),
        model,
        system_instructions: vec![SystemInstruction {
            text: "Complete the task in the current workspace. Use the Command tool to inspect, edit, and verify your work. Do not stop until the task verifier should pass.".into(),
            cache_hint: None,
        }],
        reasoning: None,
        tools,
        execution,
    };
    let ports =
        AgentExecutionPorts::new(provider, tool_context, gateway.clone(), gateway.snapshot());
    let kernel = AgentLoop::builder()
        .max_iterations(config.max_iterations)
        .max_retries(3)
        .build();
    let mut recorder = TrajectoryRecorder::new(
        config.session_id,
        format!("{}/{}", config.provider_id, config.model_id),
        request
            .system_instructions
            .iter()
            .map(|instruction| instruction.text.clone()),
        config.instruction,
    );
    let mut events = Box::pin(run_stream(&kernel, request, ports));
    while let Some(event) = events.next().await {
        recorder.record(event)?;
    }
    recorder.finish()
}

/// Executes inside the sandbox selected and enforced by Harbor.
///
/// This adapter is never used by production Runtime. Its isolation statement
/// is valid only because Harbor starts the entire Agent process inside the
/// task container before this executor is constructed.
#[derive(Debug, Clone, Copy)]
struct HarborContainerExecutor;

#[async_trait]
impl WorkspaceExecutor for HarborContainerExecutor {
    fn process_isolation(&self) -> ProcessIsolation {
        ProcessIsolation::restricted()
    }

    async fn read_file(
        &self,
        _target: &WorkspaceTarget,
        _relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        Err(WorkspaceExecutorError::InvalidRequest(
            "Harbor adapter exposes workspace access through Command only".into(),
        ))
    }

    async fn write_file(
        &self,
        _target: &WorkspaceTarget,
        _relative_path: &str,
        _content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        Err(WorkspaceExecutorError::InvalidRequest(
            "Harbor adapter exposes workspace access through Command only".into(),
        ))
    }

    async fn run_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let mut process = tokio::process::Command::new("sh");
        process
            .args(["-lc", command])
            .current_dir(&target.workspace_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let output = tokio::time::timeout(timeout, process.output())
            .await
            .map_err(|_| WorkspaceExecutorError::Timeout(timeout))??;
        let stdout_total_bytes = output.stdout.len() as u64;
        let stderr_total_bytes = output.stderr.len() as u64;
        Ok(WorkspaceCommandOutput {
            success: output.status.success(),
            status_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_total_bytes,
            stderr_total_bytes,
        })
    }
}
