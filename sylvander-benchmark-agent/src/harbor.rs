//! Harbor task-environment composition for the Sylvander Agent kernel.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use sylvander_agent::event::AgentEvent;
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
use tokio::io::AsyncReadExt as _;

use crate::{ProviderAudit, RecorderError, Trajectory, TrajectoryRecorder, persist_trajectory};

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
    pub environment_isolated: bool,
    pub trajectory_path: std::path::PathBuf,
    pub provider_audit: ProviderAudit,
}

pub async fn run_harbor_task(
    provider: Arc<dyn ModelProvider>,
    config: HarborRunConfig,
) -> Result<Trajectory, RecorderError> {
    if !config.environment_isolated {
        return Err(RecorderError::HarnessNotIsolated);
    }
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
    let mut system_instructions = vec![SystemInstruction {
        text: "Complete the task in the current workspace. Use the Command tool to inspect, edit, and verify your work. Do not stop until the task verifier should pass.".into(),
        cache_hint: None,
    }];
    if let Some(tool_guidelines) = tools.prompt_guidelines() {
        system_instructions.push(SystemInstruction {
            text: tool_guidelines,
            cache_hint: None,
        });
    }
    let request = AgentTurnRequest {
        conversation: sylvander_agent::conversation::ConversationSnapshot::new(vec![
            ChatMessage::user(&config.instruction),
        ]),
        model,
        system_instructions,
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
    )
    .with_provider_audit(config.provider_audit);
    recorder.checkpoint(&config.trajectory_path).await?;
    let mut events = Box::pin(run_stream(&kernel, request, ports));
    while let Some(event) = events.next().await {
        let checkpoint = requires_checkpoint(&event);
        let record_result = recorder.record(event);
        if checkpoint {
            recorder.checkpoint(&config.trajectory_path).await?;
        }
        record_result?;
    }
    let trajectory = recorder.finish()?;
    persist_trajectory(&config.trajectory_path, &trajectory).await?;
    Ok(trajectory)
}

fn requires_checkpoint(event: &AgentEvent) -> bool {
    !matches!(
        event,
        AgentEvent::TextChunk(_)
            | AgentEvent::ThinkingChunk(_)
            | AgentEvent::ToolCallOutputDelta { .. }
    )
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
            .process_group(0)
            .kill_on_drop(true);
        let mut child = process.spawn()?;
        let mut process_group = ProcessGroupGuard::new(child.id());
        let mut stdout = child.stdout.take().ok_or_else(|| {
            WorkspaceExecutorError::InvalidRequest("command stdout was not piped".into())
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| {
            WorkspaceExecutorError::InvalidRequest("command stderr was not piped".into())
        })?;
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        let collect = async {
            let (_, _, status) = tokio::try_join!(
                stdout.read_to_end(&mut stdout_bytes),
                stderr.read_to_end(&mut stderr_bytes),
                child.wait(),
            )?;
            Ok::<_, std::io::Error>(status)
        };
        let status = if let Ok(status) = tokio::time::timeout(timeout, collect).await {
            status?
        } else {
            // Keep the shell leader alive until the whole group has been
            // signalled; otherwise its descendants can be re-parented before
            // the group kill reaches them.
            process_group.terminate();
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(WorkspaceExecutorError::Timeout(timeout));
        };
        process_group.disarm();
        let stdout_total_bytes = stdout_bytes.len() as u64;
        let stderr_total_bytes = stderr_bytes.len() as u64;
        Ok(WorkspaceCommandOutput {
            success: status.success(),
            status_code: status.code(),
            stdout: stdout_bytes,
            stderr: stderr_bytes,
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_total_bytes,
            stderr_total_bytes,
        })
    }
}

struct ProcessGroupGuard(Option<u32>);

impl ProcessGroupGuard {
    fn new(process_group: Option<u32>) -> Self {
        Self(process_group)
    }

    fn terminate(&mut self) {
        if let Some(process_group) = self.0.take()
            && let Ok(process_group) = i32::try_from(process_group)
        {
            let _ = killpg(Pid::from_raw(process_group), Signal::SIGKILL);
        }
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}
