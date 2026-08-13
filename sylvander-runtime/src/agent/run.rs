//! Runtime-owned Session orchestration around the Agent execution kernel.
//!
//! [`AgentRun`] is a running agent instance. It is a cheap `Clone` handle
//! to shared state (`AgentRunInner`).
//!
//! # Memory: mechanism first, tools second
//!
//! Memory is agent infrastructure. The read path is exposed as a tool so the
//! model can autonomously retrieve context. Model-proposed writes enter the
//! Runtime-owned Guardian candidate flow.
//! [`AgentRun::remember`] is a separate,
//! synchronous relationship-only API for trusted application observations.
//!
//! # Session: engineering layer, model-invisible
//!
//! Sessions are purely for message routing and context isolation. The
//! model never sees session IDs.
//!
//! # Approval (M12)
//!
//! Tool approval flows through the bus. When approval is needed, the
//! loop pauses (via [`ApprovalGate`]) and the engine processes
//! `ApproveTool` responses concurrently via spawned `handle_message`
//! tasks. Per-session locks prevent concurrent execution on the same
//! session.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tracing::{Instrument as _, info, warn};

use sylvander_api::{
    PlatformAuthStatus, PlatformFeature, PlatformFeatureKind, PlatformFeatureStatus, PlatformTrust,
};
use sylvander_llm_core::{
    CacheHint, ChatMessage, ChatRole, ContentBlock, ImageContent, MediaSource, ModelCapabilities,
    ModelInfo, ModelProvider, ModelResponse, ReasoningConfig,
    ReasoningEffort as ProviderReasoningEffort, SystemInstruction, TokenUsage,
};

use crate::agent::approval::{
    ApprovalGrantContext, ApprovalGrantKey, ApprovalMemory, approval_policy_revision,
};
use crate::agent_definition::{AgentId, AgentSpec, SessionId};
use crate::execution::RuntimeExecutionService;
use crate::observability::{
    RuntimeEvent, RuntimeFailureKind, RuntimeObservability, RuntimePersistenceOperation,
    RuntimeToolFailureKind,
};
use crate::prompt_contract::{agent_model_selection, public_prompt_manifest};
use crate::session::{SessionContext, SessionMetadata, now_secs};
use crate::storage::artifact::{ArtifactTurnBinding, RuntimeArtifactService};
use crate::storage::session::{
    MessageRole as StoredMessageRole, ReplacementMessage, SessionLifetime, SessionStore,
    SessionStoreError, StoredSession, TurnCompletion, TurnFailureKind, TurnStart, TurnState,
};
use crate::storage::workspace_journal::{RollbackPreview, RollbackReport, WorkspaceJournal};
use sylvander_agent::approval::{
    ApprovalBatchResult, ApprovalDecision, ApprovalGate, ToolUseRequest,
};
use sylvander_agent::ask_user_gate::AskUserGate;
use sylvander_agent::compress::error::{CompactionError, CompactionFailureCode};
use sylvander_agent::compress::layer::CompressionLayer;
use sylvander_agent::execution_ports::AgentExecutionPorts;
use sylvander_agent::kernel::agent_loop::{self, AgentLoop};
use sylvander_agent::memory::curated::{
    CuratedContextProvider, CuratedContextSubject, CuratedMemoryScope,
};
use sylvander_agent::memory::store::{
    MemoryAppend, MemoryEntry, MemoryExecutionContext, MemoryFilter, MemoryStore, MemoryStoreError,
};
use sylvander_agent::plan_gate::{PlanDecision, PlanGate};
use sylvander_agent::prompt::{PromptResolver, SHARED_SAFETY_PROMPT};
use sylvander_agent::task_gate::TaskGate;
use sylvander_agent::tool::invocation::{
    CapabilityFeatureKind, ToolInvocationDescriptor, ToolInvocationGateway,
};
use sylvander_agent::tool::{
    RegisteredTool, ToolRegistry, ToolSourceFeature, ToolSourceKind, ToolSourceStatus,
};
use sylvander_agent::tool_context::{Cap, NetworkPolicy, ToolContext};
use sylvander_agent::tools::{MemoryReadTool, ReadTool};
use sylvander_agent::turn::conversation::ConversationSnapshot;
use sylvander_agent::turn::error::AgentLoopError;
use sylvander_agent::turn::event::ModelRetryCause;
use sylvander_agent::turn::execution_context::{AgentExecutionContext, ExecutionWorkspace};
use sylvander_agent::turn::identity::{
    AgentId as KernelAgentId, SessionId as KernelSessionId, UserId as KernelUserId,
};
use sylvander_agent::turn::request::AgentTurnRequest;
use sylvander_agent::turn_context::{
    TurnContextBudgets, TurnContextCandidate, TurnContextInputs, TurnContextLayerKind,
    TurnContextManifest, TurnContextProvenance, TurnContextSource, compose_turn_context,
    retrieve_relationship_context, retrieve_workspace_context,
};
use sylvander_agent::user_profile_prompt::{UserProfilePromptLayer, compose_user_profile_prompt};
use sylvander_agent::user_profile_provider::{UserProfileProvider, UserProfileSubject};
use sylvander_agent::workspace_executor::{
    MountedWorkspace, UnavailableExecutor, WorkspaceCapabilities, WorkspaceExecutor,
    WorkspaceRouter, WorkspaceTarget,
};
use sylvander_agent::workspace_journal::WorkspaceMutationJournal;
use sylvander_api::{
    AgentStatus as BusAgentStatus, BusMessage, MessageKind, Sender, StreamEvent, SystemMessage,
    ToolCallInfo,
};
use sylvander_channel::{MessageBus, SubscriptionFilter};

#[path = "workspace_context.rs"]
mod workspace_context;

/// Translate an authenticated API decision into the Agent kernel decision.
///
/// Protocol types terminate at this Runtime boundary; the Agent kernel only
/// receives its provider-neutral domain decision.
fn agent_plan_decision(decision: &sylvander_api::PlanDecision) -> PlanDecision {
    match decision {
        sylvander_api::PlanDecision::Approved => PlanDecision::Approved,
        sylvander_api::PlanDecision::Revised { steps } => PlanDecision::Revised {
            steps: steps.clone(),
        },
        sylvander_api::PlanDecision::Rejected { reason } => PlanDecision::Rejected {
            reason: reason.clone(),
        },
    }
}

/// Translate an internal retry classification into the versioned public API.
fn public_retry_cause(cause: ModelRetryCause) -> sylvander_api::RetryCause {
    match cause {
        ModelRetryCause::RateLimit => sylvander_api::RetryCause::RateLimit,
        ModelRetryCause::Server => sylvander_api::RetryCause::Server,
        ModelRetryCause::Network => sylvander_api::RetryCause::Network,
        ModelRetryCause::Stream => sylvander_api::RetryCause::Stream,
        ModelRetryCause::Other => sylvander_api::RetryCause::Other,
    }
}

/// Translate Agent execution facts to Runtime's current public inspection DTO.
fn public_tool_feature(feature: ToolSourceFeature) -> PlatformFeature {
    PlatformFeature {
        kind: match feature.kind {
            ToolSourceKind::Mcp => PlatformFeatureKind::Mcp,
            ToolSourceKind::Hook => PlatformFeatureKind::Hook,
            ToolSourceKind::Extension => PlatformFeatureKind::Extension,
        },
        name: feature.name,
        status: match feature.status {
            ToolSourceStatus::Active => PlatformFeatureStatus::Active,
            ToolSourceStatus::Configured => PlatformFeatureStatus::Configured,
            ToolSourceStatus::Degraded => PlatformFeatureStatus::Degraded,
            ToolSourceStatus::Unavailable => PlatformFeatureStatus::Unavailable,
        },
        summary: feature.summary,
        source: feature.source,
        trust: Some(match feature.kind {
            ToolSourceKind::Hook => PlatformTrust::User,
            ToolSourceKind::Mcp | ToolSourceKind::Extension => PlatformTrust::External,
        }),
        auth: if feature.requires_authentication {
            PlatformAuthStatus::Configured
        } else {
            PlatformAuthStatus::NotRequired
        },
        capabilities: feature.capabilities,
        reloadable: feature.reloadable,
    }
}

fn turn_system_instructions(
    system_prompt: &str,
    model: &ModelInfo,
    tools: &ToolRegistry,
) -> Vec<SystemInstruction> {
    let cache_hint = model
        .capabilities
        .contains(ModelCapabilities::PROMPT_CACHING)
        .then_some(CacheHint::Ephemeral);
    let mut instructions = vec![SystemInstruction {
        text: system_prompt.to_owned(),
        cache_hint,
    }];
    if let Some(tool_guidelines) = tools.prompt_guidelines() {
        instructions.push(SystemInstruction {
            text: tool_guidelines,
            cache_hint,
        });
    }
    instructions
}

// ---------------------------------------------------------------------------
// AgentRun (Arc-based, cheap clone)
// ---------------------------------------------------------------------------

/// Shared state for a running agent.
pub(crate) struct AgentRunInner {
    /// Unique agent identifier.
    id: AgentId,
    /// The spec this agent was built from.
    #[allow(dead_code)]
    spec: AgentSpec,
    /// The pre-built loop configuration.
    loop_config: AgentLoop,
    /// Provider-neutral exact-model router injected by Runtime.
    model_provider: Arc<dyn ModelProvider>,
    /// Agent-definition prompt before per-turn context composition.
    system_prompt: Option<String>,
    /// Stable tool catalog frozen separately for every execution.
    tools: ToolRegistry,
    /// Runtime-owned tool authorization and audit boundary.
    invocation_gateway: Arc<dyn sylvander_agent::tool::invocation::ToolInvocationGateway>,
    /// Runtime-installed, provider-neutral Session extensions. Dynamic
    /// sources remain live for the Session; turn admission freezes one exact
    /// catalog and builds its matching authorization gateway.
    session_tool_surfaces: RwLock<HashMap<SessionId, SessionToolSurface>>,
    /// Mutable selection read once at the start of every turn. Active turns
    /// keep their cloned `AgentLoop` and are never mutated underneath.
    runtime_models: RwLock<RuntimeModels>,
    runtime_permissions: RwLock<sylvander_api::PermissionProfile>,
    prompt_resolver: Option<Arc<PromptResolver>>,
    user_profile_provider: Option<Arc<dyn UserProfileProvider>>,
    curated_context_provider: Option<Arc<dyn CuratedContextProvider>>,
    turn_context_budgets: TurnContextBudgets,
    turn_context_manifests: RwLock<HashMap<SessionId, TurnContextManifest>>,
    /// Last provider-confirmed prompt usage for each session. This is window
    /// occupancy, unlike the durable cumulative billing counters.
    context_usage: RwLock<HashMap<SessionId, ContextUsage>>,
    workspace_journal: Option<Arc<WorkspaceJournal>>,
    /// Immutable Runtime-owned execution environment registry.
    execution_service: RuntimeExecutionService,
    /// Governed artifact factory; concrete storage remains outside Agent.
    artifact_service: Option<RuntimeArtifactService>,
    skill_features: std::sync::RwLock<Vec<sylvander_api::PlatformFeature>>,
    /// Handle to the message bus.
    bus: Arc<dyn MessageBus>,
    /// Runtime-owned mandatory lifecycle recorder shared across every Agent.
    observability: RuntimeObservability,
    /// Per-session conversation state.
    sessions: RwLock<HashMap<SessionId, SessionContext>>,
    /// Sessions whose identity was admitted through this run's private issuer.
    authenticated_sessions: RwLock<HashSet<SessionId>>,
    /// Permanently switches this run from legacy bus admission to Runtime
    /// issuer admission after the first authenticated lease.
    ///
    /// Engine bookkeeping still emits legacy `JoinSession` messages. Once the
    /// private issuer is active those messages are notifications only: they
    /// cannot recreate a compensated session or let a transport forge
    /// admission. Legacy-only runs never activate this boundary.
    authenticated_session_authority_active: AtomicBool,
    session_authority: Arc<SessionAuthorityMarker>,
    /// Optional durable source of truth shared with channels/runtime.
    session_store: Option<Arc<dyn SessionStore>>,
    /// Long-term memory store.
    memory: Option<Arc<dyn MemoryStore>>,
    /// Truth about how the active memory backend was selected.
    memory_source: MemorySource,
    /// Whether bus-based approval is enabled (opt-in, off by default).
    approval_enabled: bool,
    /// Static approval rules (auto-approve/auto-reject).
    approval_rules: Vec<sylvander_agent::approval::ApprovalRule>,
    /// Pending approval requests (shared with `BusApprovalGate`).
    pending_approvals: Arc<Mutex<HashMap<(SessionId, String), PendingApproval>>>,
    /// Agent-owned approval memory. Session grants are isolated by session;
    /// persistent grants exist only when the operator configured a store.
    approval_memory: Arc<Mutex<ApprovalMemory>>,
    /// Pending `AskUser` answers (shared with `BusAskUserGate`).
    pending_answers: Arc<Mutex<HashMap<(SessionId, String), PendingAnswer>>>,
    /// Pending typed plan decisions (shared with `BusPlanGate`).
    pending_plans: Arc<Mutex<HashMap<(SessionId, String), PendingPlan>>>,
    /// Independently cancellable read-only background runs.
    background_tasks: Arc<Mutex<HashMap<String, ActiveBackgroundTask>>>,
    /// Per-session concurrency locks (M12).
    session_locks: Mutex<HashMap<SessionId, Arc<Mutex<()>>>>,
    /// One cancellation sender per session that currently owns its execution
    /// lock. Queued turns do not replace the active sender.
    active_turns: Mutex<HashMap<SessionId, ActiveTurn>>,
}

#[derive(Clone)]
struct SessionToolSurface {
    extensions: ToolRegistry,
    invocation_gateway_factory: SessionInvocationGatewayFactory,
}

pub(crate) type SessionInvocationGatewayFactory = Arc<
    dyn Fn(Vec<ToolInvocationDescriptor>) -> Result<Arc<dyn ToolInvocationGateway>, AgentRunError>
        + Send
        + Sync,
>;

fn validate_tool_gateway_surface(
    expected: &[ToolInvocationDescriptor],
    invocation_gateway: &dyn ToolInvocationGateway,
) -> Result<(), AgentRunError> {
    let actual = invocation_gateway.snapshot();
    if expected
        .iter()
        .any(|descriptor| !actual.authorizes(&descriptor.name, descriptor.class))
        || actual
            .features()
            .iter()
            .filter(|feature| matches!(feature.kind, CapabilityFeatureKind::Executable(_)))
            .count()
            != expected.len()
    {
        return Err(AgentRunError::Configuration(
            "Session tool registry and authorization gateway differ".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MemorySource {
    None,
    RuntimeInjected,
}

struct PendingApproval {
    session_id: SessionId,
    grant: ApprovalGrantKey,
    persistent_identity_authorized: bool,
    allowed_scopes: Vec<sylvander_api::ApprovalScope>,
    sender: oneshot::Sender<sylvander_agent::approval::ApprovalDecision>,
}

struct PendingAnswer {
    session_id: SessionId,
    sender: oneshot::Sender<Vec<String>>,
}

struct PendingPlan {
    session_id: SessionId,
    sender: oneshot::Sender<PlanDecision>,
}

struct ActiveBackgroundTask {
    session_id: SessionId,
    cancel: oneshot::Sender<()>,
}

struct ActiveTurn {
    id: uuid::Uuid,
    interrupt: oneshot::Sender<()>,
}

#[derive(Clone)]
struct RuntimeModel {
    selection: sylvander_api::ModelSelection,
    shadow: ModelInfo,
    exact: Option<ModelInfo>,
    lifecycle: sylvander_api::ModelLifecycle,
    pricing: Option<sylvander_api::ModelPricing>,
}

struct RuntimeModels {
    available: HashMap<sylvander_api::ModelSelection, RuntimeModel>,
    current: sylvander_api::ModelSelection,
    reasoning_effort: sylvander_api::ReasoningEffort,
}

#[derive(Debug, Clone, Copy, Default)]
struct ContextUsage {
    used: u32,
    cache_read: u32,
    cache_write: u32,
}

impl RuntimeModels {
    fn public_info(&self) -> sylvander_api::RuntimeModelInfo {
        let mut models = self
            .available
            .values()
            .map(|model| {
                let reasoning_efforts = if model
                    .shadow
                    .capabilities
                    .contains(ModelCapabilities::REASONING)
                {
                    vec![
                        sylvander_api::ReasoningEffort::Off,
                        sylvander_api::ReasoningEffort::Low,
                        sylvander_api::ReasoningEffort::Medium,
                        sylvander_api::ReasoningEffort::High,
                    ]
                } else {
                    vec![sylvander_api::ReasoningEffort::Off]
                };
                sylvander_api::ModelDescriptor {
                    id: model.selection.model_id.clone(),
                    provider: model.selection.provider_id.clone(),
                    capabilities: u8::try_from(model.shadow.capabilities.bits()).unwrap_or(u8::MAX),
                    capability_names: public_capability_names(model.shadow.capabilities),
                    reasoning_efforts,
                    lifecycle: model.lifecycle.clone(),
                    pricing: model.pricing,
                }
            })
            .collect::<Vec<_>>();
        models.sort_by(|left, right| (&left.provider, &left.id).cmp(&(&right.provider, &right.id)));
        sylvander_api::RuntimeModelInfo {
            current_model: self.current.model_id.clone(),
            reasoning_effort: self.reasoning_effort,
            models,
        }
    }
}

fn public_capability_names(capabilities: ModelCapabilities) -> Vec<sylvander_api::ModelCapability> {
    [
        (
            ModelCapabilities::REASONING,
            sylvander_api::ModelCapability::ExtendedThinking,
        ),
        (
            ModelCapabilities::PROMPT_CACHING,
            sylvander_api::ModelCapability::PromptCaching,
        ),
        (
            ModelCapabilities::STRUCTURED_OUTPUT,
            sylvander_api::ModelCapability::StructuredOutput,
        ),
        (
            ModelCapabilities::TOOL_USE,
            sylvander_api::ModelCapability::ToolUse,
        ),
        (
            ModelCapabilities::VISION,
            sylvander_api::ModelCapability::Vision,
        ),
        (
            ModelCapabilities::DOCUMENT_INPUT,
            sylvander_api::ModelCapability::DocumentInput,
        ),
    ]
    .into_iter()
    .filter_map(|(flag, name)| capabilities.contains(flag).then_some(name))
    .collect()
}

fn usage_cost_nano_usd(pricing: sylvander_api::ModelPricing, usage: &TokenUsage) -> Option<u64> {
    fn component(tokens: u64, rate: u64) -> u128 {
        // rate is micro-USD / 1M tokens; nano-USD therefore divides by 1,000.
        (u128::from(tokens) * u128::from(rate) + 500) / 1_000
    }

    let cache_write = usage.cache_write_tokens.unwrap_or(0);
    let cache_read = usage.cache_read_tokens.unwrap_or(0);
    let mut total = component(usage.input_tokens, pricing.input_usd_micros_per_million)
        + component(usage.output_tokens, pricing.output_usd_micros_per_million);
    if cache_write > 0 {
        total += component(cache_write, pricing.cache_write_usd_micros_per_million?);
    }
    if cache_read > 0 {
        total += component(cache_read, pricing.cache_read_usd_micros_per_million?);
    }
    total.try_into().ok()
}

/// A running agent instance — cheap `Clone` handle.
#[derive(Clone)]
pub struct AgentRun {
    pub(crate) inner: Arc<AgentRunInner>,
}

#[derive(Debug)]
struct SessionAuthorityMarker;

/// Runtime-owned issuer for authenticated sessions on exactly one [`AgentRun`].
///
/// The matching marker is never exposed by `AgentRun`; obtaining a raw run or
/// publishing `JoinSession` on the bus cannot mint this authority.
#[derive(Clone)]
pub struct AgentSessionIssuer {
    authority: Arc<SessionAuthorityMarker>,
}

/// A single-use, run-bound admission capability.
pub struct AuthenticatedSessionLease {
    authority: Arc<SessionAuthorityMarker>,
    session_id: SessionId,
    metadata: SessionMetadata,
}

/// Proof that a session was admitted by the issuer belonging to this run.
#[derive(Debug)]
pub struct AuthenticatedSession {
    authority: Arc<SessionAuthorityMarker>,
    session_id: SessionId,
}

impl AuthenticatedSession {
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.session_id
    }
}

impl AgentSessionIssuer {
    /// Issue a capability after rejecting unsafe identity metadata. Identity
    /// authorization comes from possession of this issuer, not these strings.
    pub fn issue(
        &self,
        session_id: SessionId,
        metadata: SessionMetadata,
    ) -> Result<AuthenticatedSessionLease, AgentRunError> {
        validate_identity_component("session id", &session_id.0, 128)?;
        validate_identity_component("user id", &metadata.user_id, 256)?;
        if metadata.name.len() > 200 || metadata.name.chars().any(char::is_control) {
            return Err(AgentRunError::Authentication("invalid session name".into()));
        }
        Ok(AuthenticatedSessionLease {
            authority: self.authority.clone(),
            session_id,
            metadata,
        })
    }
}

fn validate_identity_component(
    label: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), AgentRunError> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(AgentRunError::Authentication(format!("invalid {label}")));
    }
    Ok(())
}

impl AgentRun {
    /// Build a run around an immutable provider-qualified router.
    #[must_use]
    pub fn qualified_router_builder(
        spec: AgentSpec,
        router: Arc<dyn ModelProvider>,
        model: ModelInfo,
    ) -> AgentRunBuilder {
        AgentRunBuilder::new_qualified_router(spec, router, model)
    }

    /// Unique agent identifier.
    #[must_use]
    pub fn id(&self) -> &AgentId {
        &self.inner.id
    }

    pub async fn runtime_model_info(&self) -> sylvander_api::RuntimeModelInfo {
        let runtime = self.inner.runtime_models.read().await;
        runtime.public_info()
    }

    /// Select one exact provider-qualified model for subsequently started turns.
    pub async fn select_qualified_model(
        &self,
        selection: sylvander_api::ModelSelection,
        reasoning_effort: sylvander_api::ReasoningEffort,
    ) -> Result<sylvander_api::RuntimeModelInfo, String> {
        let mut runtime = self.inner.runtime_models.write().await;
        let model = runtime.available.get(&selection).cloned().ok_or_else(|| {
            format!(
                "model `{}/{}` is not available",
                selection.provider_id, selection.model_id
            )
        })?;
        AgentRunInner::validate_turn_model(&model, reasoning_effort)
            .map_err(|error| error.to_string())?;
        if reasoning_effort != sylvander_api::ReasoningEffort::Off
            && !model
                .shadow
                .capabilities
                .contains(ModelCapabilities::REASONING)
        {
            return Err(format!(
                "model `{}` does not support reasoning effort",
                selection.model_id
            ));
        }
        runtime.current = selection;
        runtime.reasoning_effort = reasoning_effort;
        Ok(runtime.public_info())
    }

    pub async fn permission_profile(&self) -> sylvander_api::PermissionProfile {
        self.inner.runtime_permissions.read().await.clone()
    }

    /// Return redacted, read-only platform truth for UI inspection. This does
    /// not probe or start optional services and never exposes MCP environment
    /// values or memory store paths.
    #[must_use]
    pub fn platform_snapshot(&self) -> sylvander_api::PlatformSnapshot {
        let mut features = self
            .inner
            .spec
            .tools
            .iter()
            .filter_map(|tool| {
                let crate::agent_definition::ToolRef::McpServer(server) = tool else {
                    return None;
                };
                Some(PlatformFeature {
                    kind: PlatformFeatureKind::Mcp,
                    name: server.name.clone(),
                    status: PlatformFeatureStatus::Configured,
                    summary: "configured; MCP runtime health is not available".into(),
                    source: std::path::Path::new(&server.command)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_string),
                    trust: Some(PlatformTrust::External),
                    auth: if server.envs.is_empty() {
                        PlatformAuthStatus::NotRequired
                    } else {
                        PlatformAuthStatus::Configured
                    },
                    capabilities: Vec::new(),
                    reloadable: false,
                })
            })
            .collect::<Vec<_>>();

        for runtime_feature in self
            .inner
            .tools
            .platform_features()
            .into_iter()
            .map(public_tool_feature)
        {
            if let Some(existing) = features.iter_mut().find(|feature| {
                feature.kind == runtime_feature.kind && feature.name == runtime_feature.name
            }) {
                *existing = runtime_feature;
            } else {
                features.push(runtime_feature);
            }
        }
        features.extend(self.inner.skill_features.read().unwrap().clone());

        if self.inner.memory_source == MemorySource::RuntimeInjected {
            features.push(PlatformFeature {
                kind: PlatformFeatureKind::Memory,
                name: "runtime memory".into(),
                status: PlatformFeatureStatus::Active,
                summary: "long-term memory is available".into(),
                source: Some("runtime injection".into()),
                trust: Some(PlatformTrust::BuiltIn),
                auth: PlatformAuthStatus::NotRequired,
                capabilities: vec!["search".into(), "system_write".into()],
                reloadable: false,
            });
        }
        for store in &self.inner.spec.memory_stores {
            features.push(PlatformFeature {
                kind: PlatformFeatureKind::Memory,
                name: store.store_type.clone(),
                status: PlatformFeatureStatus::Configured,
                summary: if self.inner.memory_source == MemorySource::RuntimeInjected {
                    "declared; runtime memory is active".into()
                } else {
                    "declared; not activated by runtime".into()
                },
                source: Some("agent configuration".into()),
                trust: Some(PlatformTrust::BuiltIn),
                auth: PlatformAuthStatus::NotRequired,
                capabilities: Vec::new(),
                reloadable: false,
            });
        }
        if !self.inner.spec.ui_commands.is_empty() || !self.inner.spec.tool_presentations.is_empty()
        {
            let mut capabilities = Vec::new();
            if !self.inner.spec.tools.is_empty() {
                capabilities.push("tools".into());
            }
            if !self.inner.spec.ui_commands.is_empty() {
                capabilities.push("slash_commands".into());
            }
            if !self.inner.spec.tool_presentations.is_empty() {
                capabilities.push("tool_presentations".into());
            }
            features.push(PlatformFeature {
                kind: PlatformFeatureKind::Extension,
                name: "agent configuration".into(),
                status: PlatformFeatureStatus::Active,
                summary: format!(
                    "{} tools · {} commands · {} presentations",
                    self.inner.spec.tools.len(),
                    self.inner.spec.ui_commands.len(),
                    self.inner.spec.tool_presentations.len()
                ),
                source: Some("agent definition".into()),
                trust: Some(PlatformTrust::Workspace),
                auth: PlatformAuthStatus::NotRequired,
                capabilities,
                reloadable: false,
            });
        }

        let commands = self
            .inner
            .spec
            .ui_commands
            .iter()
            .map(|command| sylvander_api::UiCommandDescriptor {
                id: command.id.clone(),
                name: command.name.clone(),
                usage: command.usage.clone(),
                description: command.description.clone(),
                hint: command.hint.clone(),
                source: "agent configuration".into(),
                trust: PlatformTrust::Workspace,
                effect: sylvander_api::UiCommandEffect::SubmitPrompt {
                    template: command.prompt.clone(),
                },
            })
            .collect();

        let tool_presentations = self
            .inner
            .spec
            .tool_presentations
            .iter()
            .map(|presentation| sylvander_api::ToolPresentationDescriptor {
                tool_name: presentation.tool_name.clone(),
                label: presentation.label.clone(),
                kind: presentation.kind,
                target_field: presentation.target_field.clone(),
                source: "agent configuration".into(),
                trust: PlatformTrust::Workspace,
            })
            .collect();

        sylvander_api::PlatformSnapshot {
            features,
            commands,
            tool_presentations,
        }
    }

    pub async fn context_report(
        &self,
        session_id: Option<&SessionId>,
    ) -> sylvander_api::ContextReport {
        let models = self.inner.runtime_models.read().await;
        let model = models
            .available
            .get(&models.current)
            .expect("current model belongs to runtime catalog");
        let usage = match session_id {
            Some(session_id) => self
                .inner
                .context_usage
                .read()
                .await
                .get(session_id)
                .copied()
                .unwrap_or_default(),
            None => ContextUsage::default(),
        };
        let conversation_items = match session_id {
            Some(session_id) => self
                .inner
                .sessions
                .read()
                .await
                .get(session_id)
                .map_or(0, SessionContext::len),
            None => 0,
        };
        let mut sources = Vec::new();
        if !self.inner.spec.persona.system_prompt.is_empty() {
            sources.push(sylvander_api::ContextSource {
                kind: sylvander_api::ContextSourceKind::SystemPrompt,
                label: "agent instructions".into(),
                items: 1,
            });
        }
        if conversation_items > 0 {
            sources.push(sylvander_api::ContextSource {
                kind: sylvander_api::ContextSourceKind::Conversation,
                label: "conversation messages".into(),
                items: conversation_items,
            });
        }
        let tool_count = self.inner.tools.len();
        if tool_count > 0 {
            sources.push(sylvander_api::ContextSource {
                kind: sylvander_api::ContextSourceKind::Tools,
                label: "tool definitions".into(),
                items: tool_count,
            });
        }
        sylvander_api::ContextReport {
            model: model.shadow.reference.model.clone(),
            context_window: model.shadow.context_window,
            used_tokens: usage.used,
            remaining_tokens: model.shadow.context_window.saturating_sub(usage.used),
            cache_read_tokens: usage.cache_read,
            cache_write_tokens: usage.cache_write,
            sources,
        }
    }

    /// Force semantic compaction for one idle session. The per-session lock
    /// makes this mutually exclusive with turns; the caller gets an explicit
    /// error instead of silently queueing behind active work.
    pub async fn compact_session(
        &self,
        session_id: &SessionId,
    ) -> Result<sylvander_api::CompactionReport, String> {
        self.compact_session_typed(session_id)
            .await
            .map_err(|error| error.compatibility_reason().into())
    }

    async fn compact_session_typed(
        &self,
        session_id: &SessionId,
    ) -> Result<sylvander_api::CompactionReport, sylvander_agent::compress::error::CompactionError>
    {
        if self
            .inner
            .active_turns
            .lock()
            .await
            .contains_key(session_id)
        {
            return Err(CompactionError::new(CompactionFailureCode::Busy));
        }
        let lock = self.get_session_lock(session_id).await;
        let _guard = lock.lock().await;
        if self
            .inner
            .active_turns
            .lock()
            .await
            .contains_key(session_id)
        {
            return Err(CompactionError::new(CompactionFailureCode::Busy));
        }
        let mut history = self
            .inner
            .sessions
            .read()
            .await
            .get(session_id)
            .ok_or_else(|| CompactionError::new(CompactionFailureCode::SessionUnavailable))?
            .history_snapshot();
        if history.len() <= 4 {
            return Err(CompactionError::new(
                CompactionFailureCode::InsufficientHistory,
            ));
        }
        let runtime = self.inner.runtime_models.read().await;
        let model = runtime
            .available
            .get(&runtime.current)
            .cloned()
            .ok_or_else(|| CompactionError::new(CompactionFailureCode::Other))?;
        drop(runtime);
        let usage = TokenUsage {
            input_tokens: u64::from(model.shadow.context_window),
            ..TokenUsage::default()
        };
        let summarizer = sylvander_agent::compress::auto_compact_llm::ProviderAutoCompactLlm::new(
            self.inner.model_provider.clone(),
            model.exact.as_ref().unwrap_or(&model.shadow).clone(),
        );
        let mut context = sylvander_agent::compress::CompressContext {
            messages: &mut history,
            last_usage: &usage,
            model_info: &model.shadow,
            auto_compact_llm: Some(&summarizer),
            artifact_store: None,
        };
        let report = sylvander_agent::compress::layers::auto_compact::AutoCompactLayer::new()
            .with_trigger_ratio(0.0)
            .apply(&mut context)
            .await;
        if let Some(error) =
            sylvander_agent::compress::layer::first_failure_error(std::slice::from_ref(&report))
        {
            return Err(error);
        }
        let layers = vec![report];
        self.inner
            .apply_compacted_history(session_id, &history, &layers)
            .await
            .map_err(|_| CompactionError::new(CompactionFailureCode::Persistence))?;
        Ok(public_compaction_report(false, &layers))
    }

    pub(crate) async fn preview_workspace_rollback(
        &self,
        session_id: &SessionId,
    ) -> Result<RollbackPreview, String> {
        if self
            .inner
            .active_turns
            .lock()
            .await
            .contains_key(session_id)
        {
            return Err("interrupt active work before rolling back files".into());
        }
        if !self.inner.sessions.read().await.contains_key(session_id) {
            return Err(format!("unknown session: {session_id}"));
        }
        self.inner
            .workspace_journal
            .as_ref()
            .ok_or_else(|| "workspace rollback is not configured".to_string())?
            .preview_latest_turn(&session_id.0)
    }

    pub(crate) async fn rollback_workspace_latest(
        &self,
        session_id: &SessionId,
        expected_turn_id: &str,
    ) -> Result<RollbackReport, String> {
        if self
            .inner
            .active_turns
            .lock()
            .await
            .contains_key(session_id)
        {
            return Err("interrupt active work before rolling back files".into());
        }
        self.inner
            .workspace_journal
            .as_ref()
            .ok_or_else(|| "workspace rollback is not configured".to_string())?
            .rollback_latest_turn(&session_id.0, expected_turn_id)
    }

    pub async fn select_permissions(
        &self,
        profile: sylvander_api::PermissionProfile,
    ) -> Result<sylvander_api::PermissionProfile, String> {
        if profile.approval_policy == sylvander_api::ApprovalPolicy::Ask
            && !self.inner.approval_enabled
        {
            return Err("approval prompts are disabled by the server operator".into());
        }
        *self.inner.runtime_permissions.write().await = profile.clone();
        Ok(profile)
    }

    /// Return this agent's subscription filter.
    #[must_use]
    pub fn subscription_filter(&self) -> SubscriptionFilter {
        SubscriptionFilter::for_agent(self.inner.id.clone())
    }

    // -- session management --

    /// Admit a session using a capability issued for this exact run.
    pub async fn attach_authenticated_session(
        &self,
        lease: AuthenticatedSessionLease,
    ) -> Result<AuthenticatedSession, AgentRunError> {
        if !Arc::ptr_eq(&self.inner.session_authority, &lease.authority) {
            return Err(AgentRunError::Authentication(
                "session capability belongs to another agent run".into(),
            ));
        }
        // Publish authority before awaiting storage. A delayed legacy
        // JoinSession must never observe an unguarded admission window.
        self.inner
            .authenticated_session_authority_active
            .store(true, Ordering::Release);
        self.inner
            .authenticated_sessions
            .write()
            .await
            .insert(lease.session_id.clone());
        let ctx = match self
            .inner
            .restore_session_context(&lease.session_id, &lease.metadata)
            .await
        {
            Ok(context) => context,
            Err(error) => {
                self.inner
                    .authenticated_sessions
                    .write()
                    .await
                    .remove(&lease.session_id);
                return Err(error);
            }
        };
        self.inner
            .sessions
            .write()
            .await
            .insert(lease.session_id.clone(), ctx);
        Ok(AuthenticatedSession {
            authority: lease.authority,
            session_id: lease.session_id,
        })
    }

    /// Leave a session.
    pub async fn leave_session(&self, session_id: &SessionId) {
        self.inner
            .session_tool_surfaces
            .write()
            .await
            .remove(session_id);
        self.inner.sessions.write().await.remove(session_id);
        self.inner
            .authenticated_sessions
            .write()
            .await
            .remove(session_id);
        self.inner.context_usage.write().await.remove(session_id);
        self.inner
            .turn_context_manifests
            .write()
            .await
            .remove(session_id);
        self.inner
            .approval_memory
            .lock()
            .await
            .remove_session(session_id);
    }

    fn compose_session_tools(
        &self,
        extensions: &ToolRegistry,
    ) -> Result<ToolRegistry, AgentRunError> {
        self.inner
            .tools
            .compose_session_extensions(extensions)
            .map_err(|error| AgentRunError::Configuration(error.to_string()))
    }

    /// Publish a Session's neutral extensions and Runtime authorization
    /// factory. The current catalog is validated now, then every turn repeats
    /// composition and validation against one newly frozen catalog snapshot.
    pub(crate) async fn install_session_tool_extensions(
        &self,
        session_id: SessionId,
        extensions: ToolRegistry,
        invocation_gateway_factory: SessionInvocationGatewayFactory,
    ) -> Result<(), AgentRunError> {
        let tool_snapshot = self.compose_session_tools(&extensions)?;
        let expected = tool_snapshot.invocation_descriptors();
        let invocation_gateway = invocation_gateway_factory(expected.clone())?;
        validate_tool_gateway_surface(&expected, invocation_gateway.as_ref())?;
        self.inner.session_tool_surfaces.write().await.insert(
            session_id,
            SessionToolSurface {
                extensions,
                invocation_gateway_factory,
            },
        );
        Ok(())
    }

    /// List all sessions.
    pub async fn list_sessions(&self) -> Vec<SessionId> {
        self.inner.sessions.read().await.keys().cloned().collect()
    }

    /// Get a session context.
    pub async fn get_session(&self, session_id: &SessionId) -> Option<SessionContext> {
        self.inner.sessions.read().await.get(session_id).cloned()
    }

    /// Return the latest content-free typed context manifest for this
    /// authenticated session.
    pub async fn turn_context_manifest(
        &self,
        session: &AuthenticatedSession,
    ) -> Result<Option<TurnContextManifest>, AgentRunError> {
        if !Arc::ptr_eq(&self.inner.session_authority, &session.authority)
            || !self
                .inner
                .authenticated_sessions
                .read()
                .await
                .contains(&session.session_id)
        {
            return Err(AgentRunError::Authentication(
                "session is not authenticated".into(),
            ));
        }
        Ok(self
            .inner
            .turn_context_manifests
            .read()
            .await
            .get(&session.session_id)
            .cloned())
    }

    // -- message handling --

    /// Handle an incoming chat message: run the loop with streaming,
    /// publish every event to the bus.
    ///
    /// Called from a spawned task (M12) or directly (legacy).
    pub async fn handle_message(&self, msg: BusMessage) -> Result<(), AgentRunError> {
        self.inner.handle_message(msg).await
    }

    /// Main event loop.
    ///
    /// Chat messages are spawned as separate tasks so `run()` can
    /// concurrently process approval responses (M12).
    pub(crate) async fn run(self, mut inbox: mpsc::Receiver<BusMessage>) {
        // Publish initial status
        let _ = self
            .inner
            .bus
            .publish(BusMessage::system_status_update(
                self.inner.id.clone(),
                BusAgentStatus::Starting,
            ))
            .await;
        let _ = self
            .inner
            .bus
            .publish(BusMessage::system_status_update(
                self.inner.id.clone(),
                BusAgentStatus::Running,
            ))
            .await;

        while let Some(msg) = inbox.recv().await {
            match &msg.kind {
                // -- System messages --
                MessageKind::System(sys_msg) => {
                    match sys_msg {
                        SystemMessage::Stop => {
                            info!(agent_id = %self.inner.id, "received stop");
                            let mut tasks = self.inner.background_tasks.lock().await;
                            for (_, task) in tasks.drain() {
                                let _ = task.cancel.send(());
                            }
                            break;
                        }
                        SystemMessage::JoinSession {
                            session_id,
                            metadata,
                        } => {
                            if self
                                .inner
                                .authenticated_session_authority_active
                                .load(Ordering::Acquire)
                            {
                                continue;
                            }
                            let context = self
                                .inner
                                .restore_session_context(session_id, metadata)
                                .await;
                            match context {
                                Ok(context) => {
                                    self.inner
                                        .sessions
                                        .write()
                                        .await
                                        .insert(session_id.clone(), context);
                                    info!(agent_id = %self.inner.id, %session_id, "joined session");
                                }
                                Err(error) => {
                                    warn!(
                                        agent_id = %self.inner.id,
                                        %session_id,
                                        %error,
                                        "failed to join persistent session"
                                    );
                                }
                            }
                        }
                        SystemMessage::LeaveSession { session_id } => {
                            // Runtime-authenticated sessions can be revoked only
                            // through the private issuer path, never by a bus
                            // message that a transport or plugin could forge.
                            if self
                                .inner
                                .authenticated_session_authority_active
                                .load(Ordering::Acquire)
                            {
                                continue;
                            }
                            self.inner.sessions.write().await.remove(session_id);
                            self.inner
                                .authenticated_sessions
                                .write()
                                .await
                                .remove(session_id);
                            self.inner.context_usage.write().await.remove(session_id);
                            self.inner
                                .turn_context_manifests
                                .write()
                                .await
                                .remove(session_id);
                            self.inner
                                .approval_memory
                                .lock()
                                .await
                                .remove_session(session_id);
                            let mut tasks = self.inner.background_tasks.lock().await;
                            let task_ids = tasks
                                .iter()
                                .filter(|(_, task)| &task.session_id == session_id)
                                .map(|(task_id, _)| task_id.clone())
                                .collect::<Vec<_>>();
                            for task_id in task_ids {
                                if let Some(task) = tasks.remove(&task_id) {
                                    let _ = task.cancel.send(());
                                }
                            }
                            info!(agent_id = %self.inner.id, %session_id, "left session");
                        }
                        SystemMessage::StatusUpdate { .. } => {}

                        // M12: forward approval response to the waiting task
                        SystemMessage::ApproveTool {
                            call_id,
                            approved,
                            scope,
                            reason,
                        } => {
                            let request = self
                                .inner
                                .pending_approvals
                                .lock()
                                .await
                                .remove(&(msg.session_id.clone(), call_id.clone()));
                            if let Some(request) = request {
                                let decision = if *approved {
                                    if request.allowed_scopes.contains(scope) {
                                        match self
                                        .inner
                                        .approval_memory
                                        .lock()
                                        .await
                                        .remember(
                                            &request.session_id,
                                            request.grant,
                                            *scope,
                                            request.persistent_identity_authorized,
                                        )
                                        .await
                                    {
                                        Ok(()) => sylvander_agent::approval::ApprovalDecision::Approved,
                                        Err(reason) => {
                                            sylvander_agent::approval::ApprovalDecision::Rejected { reason }
                                        }
                                    }
                                    } else {
                                        sylvander_agent::approval::ApprovalDecision::Rejected {
                                            reason: format!(
                                                "approval scope `{scope:?}` is not permitted"
                                            ),
                                        }
                                    }
                                } else {
                                    sylvander_agent::approval::ApprovalDecision::Rejected {
                                        reason: normalize_rejection_reason(reason.as_deref()),
                                    }
                                };
                                let _ = request.sender.send(decision);
                            }
                        }

                        // M18: forward AskUser answer to the waiting gate
                        SystemMessage::AnswerQuestion { call_id, answer } => {
                            let mut pending = self.inner.pending_answers.lock().await;
                            if let Some(request) =
                                pending.remove(&(msg.session_id.clone(), call_id.clone()))
                            {
                                let _ = request.sender.send(vec![answer.clone()]);
                            }
                        }

                        SystemMessage::InterruptTurn { session_id } => {
                            self.inner.interrupt_turn(session_id).await;
                        }
                        SystemMessage::ResolvePlan { plan_id, decision } => {
                            let mut pending = self.inner.pending_plans.lock().await;
                            if let Some(request) =
                                pending.remove(&(msg.session_id.clone(), plan_id.clone()))
                            {
                                let _ = request.sender.send(agent_plan_decision(decision));
                            }
                        }
                        SystemMessage::CancelTask {
                            session_id,
                            task_id,
                        } => {
                            let mut tasks = self.inner.background_tasks.lock().await;
                            if tasks
                                .get(task_id)
                                .is_some_and(|task| &task.session_id == session_id)
                                && let Some(task) = tasks.remove(task_id)
                            {
                                let _ = task.cancel.send(());
                            }
                        }
                    }
                }

                // -- Chat messages → spawn as task (M12) --
                MessageKind::Chat => {
                    let sid = msg.session_id.clone();
                    {
                        let sessions = self.inner.sessions.read().await;
                        if !sessions.contains_key(&sid) {
                            warn!(agent_id = %self.inner.id, %sid, "chat for unknown session");
                            continue;
                        }
                    }

                    let inner = self.inner.clone();
                    let msg = msg.clone();
                    let lock = self.get_session_lock(&sid).await;

                    tokio::spawn(async move {
                        let _guard = lock.lock().await;
                        let turn_id = uuid::Uuid::new_v4();
                        let (interrupt, interrupted) = oneshot::channel();
                        inner.active_turns.lock().await.insert(
                            sid.clone(),
                            ActiveTurn {
                                id: turn_id,
                                interrupt,
                            },
                        );
                        let result = inner
                            .handle_message_interruptible(msg, interrupted, turn_id)
                            .await;
                        let mut active = inner.active_turns.lock().await;
                        if active.get(&sid).is_some_and(|turn| turn.id == turn_id) {
                            active.remove(&sid);
                        }
                        drop(active);
                        if let Err(e) = result {
                            warn!(error = %e, "handle_message failed");
                        }
                    });
                }

                // -- Stream events (for adapters) --
                MessageKind::Stream(_) => {}
            }
        }

        // Final status
        let _ = self
            .inner
            .bus
            .publish(BusMessage::system_status_update(
                self.inner.id.clone(),
                BusAgentStatus::Stopped,
            ))
            .await;
        info!(agent_id = %self.inner.id, "agent loop exited");
    }

    /// Get or create a per-session concurrency lock.
    async fn get_session_lock(&self, sid: &SessionId) -> Arc<Mutex<()>> {
        let mut locks = self.inner.session_locks.lock().await;
        locks
            .entry(sid.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    // -- memory --

    /// Return memory access tools (read only).
    #[must_use]
    pub fn memory_tools(&self) -> Vec<Arc<dyn RegisteredTool>> {
        match &self.inner.memory {
            Some(store) => vec![Arc::new(MemoryReadTool::new(store.clone()))],
            None => vec![],
        }
    }

    async fn memory_context_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<MemoryExecutionContext, MemoryStoreError> {
        if !self
            .inner
            .authenticated_sessions
            .read()
            .await
            .contains(session_id)
        {
            return Err(MemoryStoreError::AccessDenied);
        }
        let sessions = self.inner.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or(MemoryStoreError::AccessDenied)?;
        let execution = AgentExecutionContext::restricted_for(
            session.metadata.user_id.clone(),
            self.inner.id.0.clone(),
            session_id.0.clone(),
        );
        Ok(MemoryExecutionContext::for_runtime_worker(&execution))
    }

    /// Trusted application relationship write (NOT a model tool).
    ///
    /// Ownership is derived from a session already attached to this Agent
    /// application. Cross-scope/model-proposed learning must use the
    /// Runtime-owned Guardian candidate flow instead.
    pub async fn remember(
        &self,
        session: &AuthenticatedSession,
        content: impl Into<String>,
        tags: &[&str],
    ) -> Result<MemoryEntry, MemoryStoreError> {
        let append = tags.iter().fold(MemoryAppend::new(content), |append, tag| {
            append.with_tag(*tag)
        });
        self.remember_entry(session, append).await
    }

    /// Persist a structured, relationship-only application observation for an
    /// attached session. Caller-controlled identity and scope are absent.
    pub async fn remember_entry(
        &self,
        session: &AuthenticatedSession,
        append: MemoryAppend,
    ) -> Result<MemoryEntry, MemoryStoreError> {
        let store = self
            .inner
            .memory
            .as_ref()
            .ok_or_else(|| MemoryStoreError::Store("no memory store configured".into()))?;
        let session_id = self.authorized_session_id(session)?;
        let context = self.memory_context_for_session(session_id).await?;
        self.authorize_relationship_learning(&context).await?;
        store.append_relationship(&context, append).await
    }

    async fn authorize_relationship_learning(
        &self,
        context: &MemoryExecutionContext,
    ) -> Result<(), MemoryStoreError> {
        let provider = self
            .inner
            .user_profile_provider
            .as_ref()
            .ok_or(MemoryStoreError::AccessDenied)?;
        let subject = UserProfileSubject::from_authenticated_runtime(
            context
                .user_id()
                .cloned()
                .ok_or(MemoryStoreError::AccessDenied)?,
            context
                .agent_id()
                .cloned()
                .ok_or(MemoryStoreError::AccessDenied)?,
            context
                .session_id()
                .cloned()
                .ok_or(MemoryStoreError::AccessDenied)?,
        );
        let profile = provider
            .current_profile(&subject)
            .await
            .map_err(|_| MemoryStoreError::AccessDenied)?;
        if profile.is_some_and(|profile| profile.do_not_learn) {
            return Err(MemoryStoreError::AccessDenied);
        }
        Ok(())
    }

    /// System-driven memory lookup derived from an attached session.
    pub async fn recall(
        &self,
        session: &AuthenticatedSession,
        query: &str,
        filter: MemoryFilter,
    ) -> Result<Vec<MemoryEntry>, MemoryStoreError> {
        let store = self
            .inner
            .memory
            .as_ref()
            .ok_or_else(|| MemoryStoreError::Store("no memory store configured".into()))?;
        let session_id = self.authorized_session_id(session)?;
        let context = self.memory_context_for_session(session_id).await?;
        store.search_relationship(&context, query, filter).await
    }

    fn authorized_session_id<'a>(
        &self,
        session: &'a AuthenticatedSession,
    ) -> Result<&'a SessionId, MemoryStoreError> {
        Arc::ptr_eq(&self.inner.session_authority, &session.authority)
            .then_some(&session.session_id)
            .ok_or(MemoryStoreError::AccessDenied)
    }
}

// ---------------------------------------------------------------------------
// BusApprovalGate — bus-based approval (M12c)
// ---------------------------------------------------------------------------

/// Approval gate that publishes to the bus and waits for responses.
struct BusApprovalGate {
    bus: Arc<dyn MessageBus>,
    agent_id: AgentId,
    session_id: SessionId,
    grant_context: ApprovalGrantContext,
    persistent_identity_authorized: bool,
    pending_approvals: Arc<Mutex<HashMap<(SessionId, String), PendingApproval>>>,
    approval_memory: Arc<Mutex<ApprovalMemory>>,
}

struct DenyAllApprovalGate;

#[async_trait::async_trait]
impl ApprovalGate for DenyAllApprovalGate {
    async fn check_batch(&self, tools: &[ToolUseRequest]) -> ApprovalBatchResult {
        ApprovalBatchResult {
            decisions: tools
                .iter()
                .map(|_| ApprovalDecision::Rejected {
                    reason: "tool execution denied by runtime permission policy".into(),
                })
                .collect(),
        }
    }
}

#[async_trait::async_trait]
impl ApprovalGate for BusApprovalGate {
    async fn check_batch(&self, tools: &[ToolUseRequest]) -> ApprovalBatchResult {
        let batch_id = uuid::Uuid::new_v4().to_string();
        let mut decisions = vec![None; tools.len()];
        let mut receivers = Vec::new();
        let allowed_scopes = self
            .approval_memory
            .lock()
            .await
            .allowed_scopes(self.persistent_identity_authorized);
        let mut requested_tools = Vec::new();

        for (index, tool) in tools.iter().enumerate() {
            let grant = self.grant_context.key_for(tool);
            if self
                .approval_memory
                .lock()
                .await
                .contains(&self.session_id, &grant)
                .await
            {
                decisions[index] = Some(ApprovalDecision::Approved);
                continue;
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.pending_approvals.lock().await.insert(
                (self.session_id.clone(), tool.call_id.clone()),
                PendingApproval {
                    session_id: self.session_id.clone(),
                    grant,
                    persistent_identity_authorized: self.persistent_identity_authorized,
                    allowed_scopes: allowed_scopes.clone(),
                    sender: tx,
                },
            );
            receivers.push((index, tool.call_id.clone(), rx));
            requested_tools.push(tool);
        }

        if !requested_tools.is_empty() {
            let _ = self
                .bus
                .publish(BusMessage::stream_event(
                    self.session_id.clone(),
                    self.agent_id.clone(),
                    StreamEvent::ToolApprovalRequired {
                        batch_id,
                        tools: requested_tools
                            .into_iter()
                            .map(|tool| ToolCallInfo {
                                call_id: tool.call_id.clone(),
                                tool_name: tool.tool_name.clone(),
                                input: tool.input.clone(),
                            })
                            .collect(),
                        allowed_scopes,
                    },
                ))
                .await;
        }

        // Wait for all decisions (120s timeout each)
        for (index, call_id, rx) in receivers {
            let decision = if let Ok(Ok(decision)) =
                tokio::time::timeout(std::time::Duration::from_mins(2), rx).await
            {
                decision
            } else {
                publish_interaction_timeout(
                    &self.bus,
                    &self.session_id,
                    &self.agent_id,
                    sylvander_api::InteractionTimeoutKind::Approval,
                    &call_id,
                    120,
                    sylvander_api::TimeoutRecovery::RetryRequest,
                )
                .await;
                ApprovalDecision::Rejected {
                    reason: "approval timeout".into(),
                }
            };
            decisions[index] = Some(decision);
            self.pending_approvals
                .lock()
                .await
                .remove(&(self.session_id.clone(), call_id));
        }
        ApprovalBatchResult {
            decisions: decisions
                .into_iter()
                .map(|decision| decision.expect("every approval decision must settle"))
                .collect(),
        }
    }
}

async fn publish_interaction_timeout(
    bus: &Arc<dyn MessageBus>,
    session_id: &SessionId,
    agent_id: &AgentId,
    kind: sylvander_api::InteractionTimeoutKind,
    subject_id: &str,
    timeout_secs: u64,
    recovery: sylvander_api::TimeoutRecovery,
) {
    let _ = bus
        .publish(BusMessage::stream_event(
            session_id.clone(),
            agent_id.clone(),
            StreamEvent::InteractionTimedOut {
                kind,
                subject_id: subject_id.into(),
                timeout_secs,
                recovery,
            },
        ))
        .await;
}

fn normalize_rejection_reason(reason: Option<&str>) -> String {
    reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map_or_else(
            || "rejected by user".into(),
            |reason| reason.chars().take(500).collect(),
        )
}

fn compaction_summary(layers: &[sylvander_agent::compress::layer::LayerReport]) -> Option<String> {
    layers.iter().find_map(|layer| {
        layer
            .details
            .as_ref()?
            .get("summary")?
            .as_str()
            .map(str::to_owned)
    })
}

fn public_compaction_report(
    automatic: bool,
    layers: &[sylvander_agent::compress::layer::LayerReport],
) -> sylvander_api::CompactionReport {
    sylvander_api::CompactionReport {
        automatic,
        removed_messages: sylvander_agent::compress::layer::total_removed(layers),
        condensed_blocks: sylvander_agent::compress::layer::total_condensed(layers),
        freed_tokens: sylvander_agent::compress::layer::total_freed(layers),
        summary: compaction_summary(layers),
    }
}

// ===========================================================================
// BusAskUserGate — M18
// ===========================================================================

struct BusAskUserGate {
    bus: Arc<dyn MessageBus>,
    agent_id: AgentId,
    session_id: SessionId,
    pending_answers: Arc<Mutex<HashMap<(SessionId, String), PendingAnswer>>>,
}

#[async_trait::async_trait]
impl AskUserGate for BusAskUserGate {
    async fn ask(
        &self,
        call_id: &str,
        question: &str,
        options: Vec<String>,
        multi_select: bool,
    ) -> Vec<String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending_answers.lock().await.insert(
            (self.session_id.clone(), call_id.to_string()),
            PendingAnswer {
                session_id: self.session_id.clone(),
                sender: tx,
            },
        );

        // Publish AskUser event
        let _ = self
            .bus
            .publish(BusMessage::stream_event(
                self.session_id.clone(),
                self.agent_id.clone(),
                StreamEvent::AskUser {
                    call_id: call_id.into(),
                    question: question.into(),
                    options,
                    multi_select,
                },
            ))
            .await;

        // Wait up to 5 minutes for user reply
        let answer = if let Ok(Ok(answer)) =
            tokio::time::timeout(std::time::Duration::from_mins(5), rx).await
        {
            answer
        } else {
            publish_interaction_timeout(
                &self.bus,
                &self.session_id,
                &self.agent_id,
                sylvander_api::InteractionTimeoutKind::Question,
                call_id,
                300,
                sylvander_api::TimeoutRecovery::RetryRequest,
            )
            .await;
            Vec::new()
        };
        self.pending_answers
            .lock()
            .await
            .remove(&(self.session_id.clone(), call_id.to_string()));
        answer
    }
}

// ===========================================================================
// BusPlanGate — typed plan review
// ===========================================================================

struct BusPlanGate {
    bus: Arc<dyn MessageBus>,
    agent_id: AgentId,
    session_id: SessionId,
    pending_plans: Arc<Mutex<HashMap<(SessionId, String), PendingPlan>>>,
}

#[async_trait::async_trait]
impl PlanGate for BusPlanGate {
    async fn review(&self, plan_id: &str, steps: Vec<String>) -> PlanDecision {
        let (tx, rx) = oneshot::channel();
        self.pending_plans.lock().await.insert(
            (self.session_id.clone(), plan_id.to_string()),
            PendingPlan {
                session_id: self.session_id.clone(),
                sender: tx,
            },
        );
        let _ = self
            .bus
            .publish(BusMessage::stream_event(
                self.session_id.clone(),
                self.agent_id.clone(),
                StreamEvent::PlanProposed {
                    plan_id: plan_id.into(),
                    steps,
                    current: 0,
                },
            ))
            .await;

        let decision = if let Ok(Ok(decision)) =
            tokio::time::timeout(std::time::Duration::from_mins(5), rx).await
        {
            decision
        } else {
            publish_interaction_timeout(
                &self.bus,
                &self.session_id,
                &self.agent_id,
                sylvander_api::InteractionTimeoutKind::Plan,
                plan_id,
                300,
                sylvander_api::TimeoutRecovery::RetryRequest,
            )
            .await;
            PlanDecision::Rejected {
                reason: "plan review timed out".into(),
            }
        };
        self.pending_plans
            .lock()
            .await
            .remove(&(self.session_id.clone(), plan_id.to_string()));
        decision
    }

    async fn update(&self, plan_id: &str, steps: Vec<String>, current: usize) {
        let _ = self
            .bus
            .publish(BusMessage::stream_event(
                self.session_id.clone(),
                self.agent_id.clone(),
                StreamEvent::PlanUpdated {
                    plan_id: plan_id.into(),
                    steps,
                    current,
                },
            ))
            .await;
    }
}

// ===========================================================================
// BusTaskGate — isolated, read-only background investigation
// ===========================================================================

struct BusTaskGate {
    bus: Arc<dyn MessageBus>,
    agent_id: AgentId,
    session_id: SessionId,
    kernel: AgentLoop,
    request: AgentTurnRequest,
    ports: AgentExecutionPorts,
    tasks: Arc<Mutex<HashMap<String, ActiveBackgroundTask>>>,
}

#[async_trait::async_trait]
impl TaskGate for BusTaskGate {
    async fn start(&self, purpose: String, prompt: String) -> Result<String, String> {
        if prompt.trim().is_empty() {
            return Err("background task prompt cannot be empty".into());
        }
        let task_id = uuid::Uuid::new_v4().to_string();
        let (cancel, mut cancelled) = oneshot::channel();
        self.tasks.lock().await.insert(
            task_id.clone(),
            ActiveBackgroundTask {
                session_id: self.session_id.clone(),
                cancel,
            },
        );
        let _ = self
            .bus
            .publish(BusMessage::stream_event(
                self.session_id.clone(),
                self.agent_id.clone(),
                StreamEvent::TaskStarted {
                    task_id: task_id.clone(),
                    owner: self.agent_id.0.clone(),
                    purpose,
                },
            ))
            .await;

        let bus = self.bus.clone();
        let agent_id = self.agent_id.clone();
        let session_id = self.session_id.clone();
        let kernel = self.kernel.clone();
        let mut request = self.request.clone();
        let ports = self.ports.clone();
        let tasks = self.tasks.clone();
        let running_id = task_id.clone();
        tokio::spawn(async move {
            request.conversation = ConversationSnapshot::new(vec![ChatMessage::user(prompt)]);
            let mut stream = Box::pin(agent_loop::run_stream(&kernel, request, ports));
            let deadline = tokio::time::sleep(std::time::Duration::from_mins(10));
            tokio::pin!(deadline);
            loop {
                let event = tokio::select! {
                    biased;
                    _ = &mut cancelled => {
                        let _ = bus.publish(BusMessage::stream_event(
                            session_id.clone(),
                            agent_id.clone(),
                            StreamEvent::TaskCancelled {
                                task_id: running_id.clone(),
                                reason: "cancelled by user".into(),
                            },
                        )).await;
                        break;
                    }
                    () = &mut deadline => {
                        publish_interaction_timeout(
                            &bus,
                            &session_id,
                            &agent_id,
                            sylvander_api::InteractionTimeoutKind::Task,
                            &running_id,
                            600,
                            sylvander_api::TimeoutRecovery::NarrowScope,
                        ).await;
                        let _ = bus.publish(BusMessage::stream_event(
                            session_id.clone(),
                            agent_id.clone(),
                            StreamEvent::TaskFailed {
                                task_id: running_id.clone(),
                                error: "background task timed out after 600s".into(),
                            },
                        )).await;
                        break;
                    }
                    event = stream.next() => event,
                };
                let Some(event) = event else { break };
                let public = match event {
                    sylvander_agent::turn::event::AgentEvent::IterationStart { iteration } => {
                        Some(StreamEvent::TaskProgress {
                            task_id: running_id.clone(),
                            message: format!("iteration {iteration}"),
                        })
                    }
                    sylvander_agent::turn::event::AgentEvent::ToolCallStart { name, .. } => {
                        Some(StreamEvent::TaskProgress {
                            task_id: running_id.clone(),
                            message: format!("running {name}"),
                        })
                    }
                    sylvander_agent::turn::event::AgentEvent::Done(outcome) => {
                        Some(StreamEvent::TaskCompleted {
                            task_id: running_id.clone(),
                            summary: outcome.final_response.text(),
                        })
                    }
                    sylvander_agent::turn::event::AgentEvent::Error(error) => {
                        Some(StreamEvent::TaskFailed {
                            task_id: running_id.clone(),
                            error: error.to_string(),
                        })
                    }
                    _ => None,
                };
                let terminal = matches!(
                    public,
                    Some(StreamEvent::TaskCompleted { .. } | StreamEvent::TaskFailed { .. })
                );
                if let Some(event) = public {
                    let _ = bus
                        .publish(BusMessage::stream_event(
                            session_id.clone(),
                            agent_id.clone(),
                            event,
                        ))
                        .await;
                }
                if terminal {
                    break;
                }
            }
            tasks.lock().await.remove(&running_id);
        });
        Ok(task_id)
    }
}

// ---------------------------------------------------------------------------
// AgentRunInner — the actual implementation
// ---------------------------------------------------------------------------

impl AgentRunInner {
    fn inner_prompt_resolver(&self) -> Result<&PromptResolver, AgentRunError> {
        self.prompt_resolver
            .as_deref()
            .ok_or_else(prompt_integrity_error)
    }

    async fn load_user_profile(
        &self,
        session_id: &SessionId,
        metadata: &SessionMetadata,
    ) -> Result<Option<UserProfilePromptLayer>, AgentRunError> {
        let Some(provider) = &self.user_profile_provider else {
            return Ok(None);
        };
        if !self
            .authenticated_sessions
            .read()
            .await
            .contains(session_id)
        {
            return Err(AgentRunError::Authentication(
                "session is not authenticated".into(),
            ));
        }
        let subject = UserProfileSubject::from_authenticated_runtime(
            KernelUserId::new(metadata.user_id.clone()),
            KernelAgentId::new(self.id.0.clone()),
            KernelSessionId::new(session_id.0.clone()),
        );
        provider
            .current_profile(&subject)
            .await
            .map_err(|error| AgentRunError::Configuration(error.to_string()))?
            .map(|view| {
                compose_user_profile_prompt(&view)
                    .map_err(|error| AgentRunError::Configuration(error.to_string()))
            })
            .transpose()
    }

    fn validate_turn_model(
        model: &RuntimeModel,
        reasoning_effort: sylvander_api::ReasoningEffort,
    ) -> Result<ModelInfo, AgentRunError> {
        if reasoning_effort != sylvander_api::ReasoningEffort::Off
            && !model
                .shadow
                .capabilities
                .contains(ModelCapabilities::REASONING)
        {
            return Err(AgentRunError::Configuration(format!(
                "model `{}` does not support reasoning effort",
                model.selection.model_id
            )));
        }
        let exact = model.exact.as_ref().ok_or_else(|| {
            AgentRunError::Configuration(
                "provider-backed model selection lacks exact metadata".into(),
            )
        })?;
        if exact.reference.provider != model.selection.provider_id
            || exact.reference.model != model.selection.model_id
            || exact.reference != model.shadow.reference
        {
            return Err(AgentRunError::Configuration(format!(
                "model `{}/{}` is not routed by this Agent",
                model.selection.provider_id, model.selection.model_id
            )));
        }
        Ok(exact.clone())
    }

    async fn apply_compacted_history(
        &self,
        session_id: &SessionId,
        history: &[ChatMessage],
        layers: &[sylvander_agent::compress::layer::LayerReport],
    ) -> Result<(), AgentRunError> {
        let metadata = {
            let sessions = self.sessions.read().await;
            let Some(session) = sessions.get(session_id) else {
                return Err(AgentRunError::UnknownSession(session_id.clone()));
            };
            session.metadata.clone()
        };
        if compaction_summary(layers).is_some()
            && let Some(store) = &self.session_store
        {
            let caller = sylvander_api::SessionContext::new(
                metadata.user_id,
                self.id.clone(),
                session_id.clone(),
            );
            let mut replacement = Vec::with_capacity(history.len());
            for (index, message) in history.iter().enumerate() {
                let content = serde_json::to_value(message).map_err(|_| {
                    AgentRunError::session_persistence(
                        SessionPersistenceOperation::ReplaceHistory,
                        SessionStoreError::Invalid("compacted history serialization failed".into()),
                    )
                })?;
                let role = match message.role {
                    ChatRole::User => StoredMessageRole::User,
                    ChatRole::Assistant => StoredMessageRole::Assistant,
                };
                replacement.push(ReplacementMessage {
                    role,
                    content,
                    tool_name: (index == 0).then(|| "context_summary".into()),
                });
            }
            store
                .replace_active_history(&caller, session_id, replacement)
                .await
                .map_err(|source| {
                    AgentRunError::session_persistence(
                        SessionPersistenceOperation::ReplaceHistory,
                        source,
                    )
                })?;
        }
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| AgentRunError::UnknownSession(session_id.clone()))?;
        session.history = history.to_vec();
        session.updated_at = now_secs();
        drop(sessions);
        self.context_usage.write().await.remove(session_id);
        Ok(())
    }

    async fn restore_session_context(
        &self,
        session_id: &SessionId,
        metadata: &SessionMetadata,
    ) -> Result<SessionContext, AgentRunError> {
        let mut context = SessionContext::new(session_id.clone(), metadata.clone());
        let Some(store) = &self.session_store else {
            return Ok(context);
        };

        match store.get(session_id).await {
            Ok(None) => {
                let mut stored = StoredSession::new(
                    session_id.clone(),
                    metadata.name.clone(),
                    SessionLifetime::Persistent,
                    metadata.clone(),
                    vec![self.id.clone()],
                );
                stored.effective_config = Some(self.direct_session_config(metadata).await);
                store.save(&stored).await.map_err(|source| {
                    AgentRunError::session_persistence(
                        SessionPersistenceOperation::CreateSession,
                        source,
                    )
                })?;
            }
            Ok(Some(stored)) => {
                context.metadata = stored.metadata;
            }
            Err(source) => {
                return Err(AgentRunError::session_persistence(
                    SessionPersistenceOperation::InspectSession,
                    source,
                ));
            }
        }

        let caller = sylvander_api::SessionContext::new(
            metadata.user_id.clone(),
            self.id.clone(),
            session_id.clone(),
        );
        let messages = store
            .read_history(&caller, session_id, false, None)
            .await
            .map_err(|source| {
                AgentRunError::session_persistence(
                    SessionPersistenceOperation::RestoreHistory,
                    source,
                )
            })?;
        for stored in messages {
            let message = serde_json::from_value(stored.content).map_err(|_| {
                AgentRunError::session_persistence(
                    SessionPersistenceOperation::RestoreHistory,
                    SessionStoreError::Invalid("malformed persisted message".into()),
                )
            })?;
            context.history.push(message);
        }
        Ok(context)
    }

    async fn direct_session_config(
        &self,
        metadata: &SessionMetadata,
    ) -> sylvander_api::SessionEffectiveConfig {
        let runtime = self.runtime_models.read().await;
        let source = || sylvander_api::SessionConfigSource {
            kind: sylvander_api::SessionConfigSourceKind::AgentDefault,
            reference: Some("direct-agent".into()),
        };
        let prompt = self.system_prompt.as_deref().unwrap_or_default();
        let resolved_prompt = self.prompt_resolver.as_ref().and_then(|resolver| {
            resolver
                .resolve(&agent_model_selection(&runtime.current), None, None)
                .ok()
        });
        let (prompt_profile, system_prompt_sha256, prompt_manifest) = resolved_prompt.map_or_else(
            || {
                let sha256 = format!("{:x}", Sha256::digest(prompt.as_bytes()));
                (
                    None,
                    sha256.clone(),
                    sylvander_api::PromptManifest {
                        layers: vec![sylvander_api::PromptLayerDigest {
                            kind: sylvander_api::PromptLayerKind::Agent,
                            reference: Some("direct-agent".into()),
                            sha256: sha256.clone(),
                            byte_count: prompt.len() as u64,
                        }],
                        aggregate_sha256: sha256,
                        total_bytes: prompt.len() as u64,
                    },
                )
            },
            |resolved| {
                (
                    resolved.profile_id,
                    resolved.system_prompt_sha256,
                    public_prompt_manifest(resolved.manifest),
                )
            },
        );
        sylvander_api::SessionEffectiveConfig {
            agent_id: self.id.clone(),
            agent_revision: 1,
            provider_id: runtime.current.provider_id.clone(),
            provider_revision: 1,
            model_id: runtime.current.model_id.clone(),
            model_revision: 1,
            reasoning_effort: runtime.reasoning_effort,
            permissions: self.runtime_permissions.read().await.clone(),
            prompt_profile,
            system_prompt_sha256,
            prompt_manifest,
            agent_workspace: None,
            user_workspace: Some(sylvander_api::SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: metadata.workspace.clone(),
                read_only: false,
                instruction_focus: None,
            }),
            workspace_mounts: Vec::new(),
            execution_target: "local".into(),
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

    async fn interrupt_turn(&self, session_id: &SessionId) {
        if let Some(turn) = self.active_turns.lock().await.remove(session_id) {
            let _ = turn.interrupt.send(());
        }
    }

    async fn cancel_pending_decisions(&self, session_id: &SessionId) {
        let approval_ids = {
            let pending = self.pending_approvals.lock().await;
            pending
                .iter()
                .filter(|(_, request)| &request.session_id == session_id)
                .map(|(call_id, _)| call_id.clone())
                .collect::<Vec<_>>()
        };
        let mut approvals = self.pending_approvals.lock().await;
        for call_id in approval_ids {
            if let Some(request) = approvals.remove(&call_id) {
                let _ = request.sender.send(ApprovalDecision::Rejected {
                    reason: "turn interrupted by user".into(),
                });
            }
        }
        drop(approvals);

        let answer_ids = {
            let pending = self.pending_answers.lock().await;
            pending
                .iter()
                .filter(|(_, request)| &request.session_id == session_id)
                .map(|(call_id, _)| call_id.clone())
                .collect::<Vec<_>>()
        };
        let mut answers = self.pending_answers.lock().await;
        for call_id in answer_ids {
            if let Some(request) = answers.remove(&call_id) {
                let _ = request.sender.send(Vec::new());
            }
        }
        drop(answers);

        let plan_ids = {
            let pending = self.pending_plans.lock().await;
            pending
                .iter()
                .filter(|(_, request)| &request.session_id == session_id)
                .map(|(plan_id, _)| plan_id.clone())
                .collect::<Vec<_>>()
        };
        let mut plans = self.pending_plans.lock().await;
        for plan_id in plan_ids {
            if let Some(request) = plans.remove(&plan_id) {
                let _ = request.sender.send(PlanDecision::Rejected {
                    reason: "turn interrupted by user".into(),
                });
            }
        }
    }

    /// Core: handle a chat message. Runs the loop with streaming.
    async fn handle_message(&self, msg: BusMessage) -> Result<(), AgentRunError> {
        self.handle_message_correlated(msg, std::future::pending::<()>(), uuid::Uuid::new_v4())
            .await
    }

    async fn handle_message_interruptible(
        &self,
        msg: BusMessage,
        interrupted: oneshot::Receiver<()>,
        turn_id: uuid::Uuid,
    ) -> Result<(), AgentRunError> {
        self.handle_message_correlated(msg, interrupted, turn_id)
            .await
    }

    async fn handle_message_correlated<F>(
        &self,
        msg: BusMessage,
        interrupted: F,
        turn_id: uuid::Uuid,
    ) -> Result<(), AgentRunError>
    where
        F: std::future::Future,
    {
        let correlation = TurnCorrelation::new(&msg, turn_id);
        let session_id = msg.session_id.clone();
        let span = tracing::info_span!(
            "agent_turn",
            agent_id = %self.id,
            session_id = %msg.session_id,
            turn_id = %correlation.turn,
            request_id = %correlation.request,
            trace_id = %correlation.trace,
        );
        self.observability.record(RuntimeEvent::TurnStarted {
            request_id: correlation.request.clone(),
            trace_id: correlation.trace.clone(),
            turn_id: correlation.turn.clone(),
            session_id: session_id.clone(),
            agent_id: self.id.clone(),
        });
        async {
            info!("turn started");
            let result = self
                .handle_message_with_interrupt(msg, interrupted, &correlation.turn)
                .await;
            if let Err(error) = &result {
                if let Some(store) = &self.session_store {
                    let persisted = match store.turn(&session_id, &correlation.turn).await {
                        Ok(Some(turn)) if turn.state == TurnState::Running => Some(
                            store
                                .finish_turn(
                                    &session_id,
                                    &correlation.turn,
                                    TurnState::Failed,
                                    Some(turn_failure_kind(error)),
                                )
                                .await,
                        ),
                        Ok(_) => None,
                        Err(source) => Some(Err(source)),
                    };
                    if let Some(persisted) = persisted {
                        self.observability
                            .record(RuntimeEvent::PersistenceFinished {
                                turn_id: correlation.turn.clone(),
                                session_id: session_id.clone(),
                                operation: RuntimePersistenceOperation::FinishTurn,
                                succeeded: persisted.is_ok(),
                            });
                    }
                }
                if let AgentRunError::SessionPersistence { operation, .. } = error {
                    self.observability
                        .record(RuntimeEvent::PersistenceFinished {
                            turn_id: correlation.turn.clone(),
                            session_id: session_id.clone(),
                            operation: runtime_persistence_operation(*operation),
                            succeeded: false,
                        });
                }
                self.observability.record(RuntimeEvent::TurnFailed {
                    turn_id: correlation.turn.clone(),
                    session_id: session_id.clone(),
                    kind: runtime_failure_kind(error),
                });
                match error {
                    AgentRunError::Loop(error) => self.publish_error(&session_id, error).await,
                    AgentRunError::SessionPersistence { .. } => {
                        self.publish_stream(
                            &session_id,
                            sylvander_api::StreamEvent::Error {
                                message: error.to_string(),
                            },
                        )
                        .await;
                    }
                    AgentRunError::UnknownSession(_)
                    | AgentRunError::Authentication(_)
                    | AgentRunError::Build(_)
                    | AgentRunError::Configuration(_) => {}
                }
            }
            info!(succeeded = result.is_ok(), "turn finished");
            result
        }
        .instrument(span)
        .await
    }

    async fn handle_message_with_interrupt<F>(
        &self,
        msg: BusMessage,
        interrupted: F,
        turn_id: &str,
    ) -> Result<(), AgentRunError>
    where
        F: std::future::Future,
    {
        let session_id = msg.session_id.clone();
        let user_message = Self::message_to_param(&msg);
        let stored_session = if let Some(store) = &self.session_store {
            store.get(&session_id).await.map_err(|source| {
                AgentRunError::session_persistence(
                    SessionPersistenceOperation::InspectSession,
                    source,
                )
            })?
        } else {
            None
        };
        if let (Some(stored), Sender::User(sender)) = (&stored_session, &msg.sender)
            && sender != &stored.metadata.user_id
        {
            return Err(AgentRunError::Configuration(
                "session identity verification failed".into(),
            ));
        }
        let effective_config = stored_session
            .as_ref()
            .map(|session| {
                session.effective_config.clone().ok_or_else(|| {
                    AgentRunError::Configuration(format!(
                        "durable session {session_id} has no effective configuration"
                    ))
                })
            })
            .transpose()?;
        if let Some(effective) = &effective_config
            && effective.agent_id != self.id
        {
            return Err(AgentRunError::Configuration(format!(
                "session {session_id} is configured for Agent {}, not {}",
                effective.agent_id, self.id
            )));
        }
        let (selected_model, selected_effort) = {
            let runtime = self.runtime_models.read().await;
            let selection = effective_config.as_ref().map_or_else(
                || runtime.current.clone(),
                |config| sylvander_api::ModelSelection {
                    provider_id: config.provider_id.clone(),
                    model_id: config.model_id.clone(),
                },
            );
            let model = runtime
                .available
                .get(&selection)
                .ok_or_else(|| {
                    AgentRunError::Configuration(format!(
                        "session {session_id} selects unavailable model `{}/{}`",
                        selection.provider_id, selection.model_id
                    ))
                })?
                .clone();
            (
                model,
                effective_config
                    .as_ref()
                    .map_or(runtime.reasoning_effort, |config| config.reasoning_effort),
            )
        };
        let selected_exact_model = Self::validate_turn_model(&selected_model, selected_effort)?;
        let loop_config = self.loop_config.clone();
        let selected_pricing = selected_model.pricing;
        let session_metadata = {
            let sessions = self.sessions.read().await;
            let ctx = sessions
                .get(&session_id)
                .ok_or_else(|| AgentRunError::UnknownSession(session_id.clone()))?;
            ctx.metadata.clone()
        };
        let user_profile = self
            .load_user_profile(&session_id, &session_metadata)
            .await?;

        let mut context_inputs =
            if let (Some(stored), Some(effective)) = (&stored_session, &effective_config) {
                let prompt_policy = self.inner_prompt_resolver()?;
                let resolved_prompt = prompt_policy
                    .resolve(
                        &agent_model_selection(&selected_model.selection),
                        stored.config_overrides.prompt_profile.as_deref(),
                        stored.config_overrides.system_prompt.as_deref(),
                    )
                    .map_err(|_| prompt_integrity_error())?;
                if effective.system_prompt_sha256 != resolved_prompt.system_prompt_sha256
                    || effective.prompt_manifest
                        != public_prompt_manifest(resolved_prompt.manifest.clone())
                {
                    return Err(prompt_integrity_error());
                }
                prompt_policy
                    .turn_context_inputs(
                        &agent_model_selection(&selected_model.selection),
                        stored.config_overrides.prompt_profile.as_deref(),
                        stored.config_overrides.system_prompt.as_deref(),
                        user_profile.as_ref(),
                    )
                    .map_err(|_| prompt_integrity_error())?
            } else {
                let mut inputs = TurnContextInputs::default();
                inputs.push_required(
                    TurnContextLayerKind::Safety,
                    TurnContextCandidate::authoritative(
                        SHARED_SAFETY_PROMPT,
                        TurnContextProvenance::new(
                            TurnContextSource::RuntimeSafety,
                            "sylvander-safety:v1",
                        ),
                    ),
                );
                if let Some(prompt) = self.system_prompt.clone()
                    && !prompt.is_empty()
                {
                    inputs.push_required(
                        TurnContextLayerKind::Agent,
                        TurnContextCandidate::authoritative(
                            prompt,
                            TurnContextProvenance::new(
                                TurnContextSource::AgentDefinition,
                                format!("agent:{}", self.id),
                            ),
                        ),
                    );
                }
                if let Some(profile) = &user_profile {
                    inputs.push_required(
                        TurnContextLayerKind::UserProfile,
                        TurnContextCandidate::authoritative(
                            profile.content(),
                            TurnContextProvenance::new(
                                TurnContextSource::UserProfile,
                                profile.provenance.source,
                            )
                            .with_revision(profile.provenance.profile_revision),
                        ),
                    );
                }
                inputs
            };

        let (agent_workspace, task_workspace, workspace_mounts) =
            effective_config
                .as_ref()
                .map_or((None, None, &[][..]), |config| {
                    (
                        config.agent_workspace.as_ref(),
                        config.user_workspace.as_ref(),
                        config.workspace_mounts.as_slice(),
                    )
                });
        let workspace = workspace_turn_context(
            agent_workspace,
            task_workspace,
            workspace_mounts,
            session_metadata.workspace.as_path(),
            &self.execution_service,
            &self.skill_features,
            &msg.payload,
            self.turn_context_budgets.workspace_knowledge,
        )
        .await?;
        if let Some(authoritative) = workspace.authoritative {
            context_inputs.push_required(TurnContextLayerKind::WorkspaceKnowledge, authoritative);
        }
        context_inputs.extend_retrieved(
            TurnContextLayerKind::WorkspaceKnowledge,
            workspace.retrieved,
        );

        if let Some(memory) = self.memory.as_ref()
            && self
                .authenticated_sessions
                .read()
                .await
                .contains(&session_id)
        {
            let execution = AgentExecutionContext::restricted_for(
                session_metadata.user_id.clone(),
                self.id.0.clone(),
                session_id.0.clone(),
            );
            let memory_context = MemoryExecutionContext::for_runtime_worker(&execution);
            let relationship = retrieve_relationship_context(
                memory.as_ref(),
                &memory_context,
                &msg.payload,
                self.turn_context_budgets.relationship_memory,
                now_secs(),
            )
            .await
            .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
            context_inputs.extend_retrieved(TurnContextLayerKind::RelationshipMemory, relationship);
        }
        if let Some(provider) = self.curated_context_provider.as_ref()
            && self
                .authenticated_sessions
                .read()
                .await
                .contains(&session_id)
        {
            let mut workspace_ids = std::collections::BTreeSet::new();
            if let Some(config) = effective_config.as_ref() {
                if let Some(binding) = config.agent_workspace.as_ref() {
                    workspace_ids.insert(binding.execution_target.clone());
                }
                if let Some(binding) = config.user_workspace.as_ref() {
                    workspace_ids.insert(binding.execution_target.clone());
                }
                workspace_ids.extend(
                    config
                        .workspace_mounts
                        .iter()
                        .map(|mount| mount.binding.execution_target.clone()),
                );
            }
            let subject = CuratedContextSubject {
                user_id: KernelUserId::new(&session_metadata.user_id),
                agent_id: KernelAgentId::new(self.id.0.clone()),
                session_id: KernelSessionId::new(session_id.0.clone()),
                workspace_ids: workspace_ids.into_iter().collect(),
            };
            let max_items = self
                .turn_context_budgets
                .workspace_knowledge
                .max_items
                .saturating_add(self.turn_context_budgets.relationship_memory.max_items)
                .min(64);
            let entries = if max_items == 0 {
                Vec::new()
            } else {
                provider
                    .retrieve(&subject, &msg.payload, max_items)
                    .await
                    .map_err(|error| AgentRunError::Configuration(error.to_string()))?
            };
            for entry in entries {
                let layer = match entry.scope {
                    CuratedMemoryScope::Relationship => TurnContextLayerKind::RelationshipMemory,
                    CuratedMemoryScope::UserProfile => TurnContextLayerKind::UserProfile,
                    CuratedMemoryScope::AgentCanonical => TurnContextLayerKind::Agent,
                    CuratedMemoryScope::WorkspaceKnowledge => {
                        TurnContextLayerKind::WorkspaceKnowledge
                    }
                };
                context_inputs.extend_retrieved(
                    layer,
                    [TurnContextCandidate::retrieved(
                        entry.content,
                        TurnContextProvenance::new(
                            TurnContextSource::GuardianCurated,
                            entry.reference,
                        )
                        .with_revision(entry.revision),
                        u32::from(entry.relevance),
                    )
                    .with_expiry(entry.expires_at_unix_secs)],
                );
            }
        }

        let composed = compose_turn_context(context_inputs, &self.turn_context_budgets, now_secs())
            .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
        let system_prompt = composed.system_prompt().to_owned();
        let context_manifest = composed.manifest;

        // 1. Persist the immutable turn boundary before provider or tool work.
        let permissions = if let Some(effective) = &effective_config {
            effective.permissions.clone()
        } else {
            self.runtime_permissions.read().await.clone()
        };
        if let (Some(store), Some(stored), Some(effective)) =
            (&self.session_store, &stored_session, &effective_config)
        {
            let user_id = match &msg.sender {
                Sender::User(user_id) => user_id.as_str(),
                _ => "unix-client",
            };
            let caller =
                sylvander_api::SessionContext::new(user_id, self.id.clone(), session_id.clone());
            let user_content = serde_json::to_value(&user_message).map_err(|_| {
                AgentRunError::session_persistence(
                    SessionPersistenceOperation::BeginTurn,
                    SessionStoreError::Invalid("user message serialization failed".into()),
                )
            })?;
            store
                .begin_turn(
                    &caller,
                    TurnStart {
                        session_id: session_id.clone(),
                        turn_id: turn_id.into(),
                        config_revision: stored.config_revision,
                        effective_config: effective.clone(),
                        user_content,
                        model_id: selected_model.shadow.reference.model.clone(),
                    },
                )
                .await
                .map_err(|source| {
                    AgentRunError::session_persistence(
                        SessionPersistenceOperation::BeginTurn,
                        source,
                    )
                })?;
            self.observability
                .record(RuntimeEvent::PersistenceFinished {
                    turn_id: turn_id.to_owned(),
                    session_id: session_id.clone(),
                    operation: RuntimePersistenceOperation::BeginTurn,
                    succeeded: true,
                });
        }
        self.turn_context_manifests
            .write()
            .await
            .insert(session_id.clone(), context_manifest);
        let history = {
            let mut sessions = self.sessions.write().await;
            let ctx = sessions
                .get_mut(&session_id)
                .ok_or_else(|| AgentRunError::UnknownSession(session_id.clone()))?;
            ctx.append_user_message(user_message);
            ctx.history_snapshot()
        };

        // 2. Build per-session approval gate and tool surface from one
        // immutable permission/capability snapshot. Changes made mid-turn
        // apply to the next turn and invalidate persistent grants there.
        let session_tool_surface = self
            .session_tool_surfaces
            .read()
            .await
            .get(&session_id)
            .cloned();
        let (turn_tools, tool_surface_revision, invocation_gateway) =
            if let Some(surface) = session_tool_surface {
                let (tools, revision) = self
                    .tools
                    .compose_session_extensions(&surface.extensions)
                    .map_err(|error| AgentRunError::Configuration(error.to_string()))?
                    .freeze_for_turn();
                let descriptors = tools.invocation_descriptors();
                let gateway = (surface.invocation_gateway_factory)(descriptors.clone())?;
                validate_tool_gateway_surface(&descriptors, gateway.as_ref())?;
                (tools, revision, gateway)
            } else {
                let (tools, revision) = self.tools.freeze_for_turn();
                (tools, revision, self.invocation_gateway.clone())
            };
        let prompt_context_features = self
            .skill_features
            .read()
            .unwrap()
            .iter()
            .map(|feature| feature.name.clone())
            .collect::<Vec<_>>();
        let invocation_snapshot = invocation_gateway
            .snapshot()
            .for_turn(&tool_surface_revision, prompt_context_features);
        let capability_revision = invocation_snapshot.revision().to_owned();
        let identity_authorized = self
            .authenticated_sessions
            .read()
            .await
            .contains(&session_id);
        let mut approval_gate: Option<Arc<dyn ApprovalGate>> = None;
        if permissions.approval_policy == sylvander_api::ApprovalPolicy::Ask {
            let grant_context = ApprovalGrantContext::new(
                session_metadata.user_id.clone(),
                self.id.clone(),
                approval_policy_revision(&permissions, &self.approval_rules),
                capability_revision,
            );
            let bus_gate: Arc<dyn ApprovalGate> = Arc::new(BusApprovalGate {
                bus: self.bus.clone(),
                agent_id: self.id.clone(),
                session_id: session_id.clone(),
                grant_context,
                persistent_identity_authorized: identity_authorized,
                pending_approvals: self.pending_approvals.clone(),
                approval_memory: self.approval_memory.clone(),
            });
            let gate: Arc<dyn ApprovalGate> = if self.approval_rules.is_empty() {
                bus_gate
            } else {
                Arc::new(sylvander_agent::approval::RuleBasedApprovalGate::new(
                    self.approval_rules.clone(),
                    bus_gate,
                ))
            };
            approval_gate = Some(gate);
        }
        if permissions.approval_policy == sylvander_api::ApprovalPolicy::Deny {
            approval_gate = Some(Arc::new(DenyAllApprovalGate));
        }
        let tool_context = tool_context_for_permissions(
            ToolSessionExecution {
                metadata: &session_metadata,
                effective_config: effective_config.as_ref(),
                execution_service: &self.execution_service,
            },
            &self.id,
            &session_id,
            &permissions,
            self.memory.is_some() && identity_authorized,
            self.workspace_journal.clone(),
            Some(turn_id),
        );
        let artifact_store = self
            .artifact_service
            .as_ref()
            .map(|service| {
                service.bind(ArtifactTurnBinding {
                    user_id: session_metadata.user_id.clone(),
                    agent_id: self.id.0.clone(),
                    session_id: session_id.0.clone(),
                    turn_id: turn_id.to_owned(),
                    created_at: tool_context.execution.started_at_unix_secs,
                })
            })
            .transpose()
            .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
        let ask_user_gate: Arc<dyn AskUserGate> = Arc::new(BusAskUserGate {
            bus: self.bus.clone(),
            agent_id: self.id.clone(),
            session_id: session_id.clone(),
            pending_answers: self.pending_answers.clone(),
        });
        let plan_gate: Arc<dyn PlanGate> = Arc::new(BusPlanGate {
            bus: self.bus.clone(),
            agent_id: self.id.clone(),
            session_id: session_id.clone(),
            pending_plans: self.pending_plans.clone(),
        });
        let reasoning = selected_effort
            .budget_tokens()
            .map(|budget_tokens| ReasoningConfig {
                budget_tokens: Some(budget_tokens.min(selected_exact_model.max_output_tokens)),
                effort: Some(match selected_effort {
                    sylvander_api::ReasoningEffort::Low => ProviderReasoningEffort::Low,
                    sylvander_api::ReasoningEffort::Medium => ProviderReasoningEffort::Medium,
                    sylvander_api::ReasoningEffort::High => ProviderReasoningEffort::High,
                    sylvander_api::ReasoningEffort::Off => {
                        unreachable!("disabled reasoning cannot carry a token budget")
                    }
                }),
            });
        let system_instructions =
            turn_system_instructions(&system_prompt, &selected_exact_model, &turn_tools);
        let request = AgentTurnRequest {
            conversation: ConversationSnapshot::new(history),
            model: selected_exact_model,
            system_instructions,
            reasoning,
            tools: turn_tools,
            execution: tool_context.execution.as_ref().clone(),
        };
        let mut background_request = request.clone();
        background_request.conversation = ConversationSnapshot::default();
        background_request.tools = background_request
            .tools
            .retain_named(&[ReadTool::NAME, MemoryReadTool::NAME]);
        background_request.system_instructions = turn_system_instructions(
            &system_prompt,
            &background_request.model,
            &background_request.tools,
        );
        let mut background_ports = AgentExecutionPorts::new(
            self.model_provider.clone(),
            tool_context.clone(),
            invocation_gateway.clone(),
            invocation_snapshot.clone(),
        );
        if let Some(store) = &artifact_store {
            background_ports = background_ports.with_artifact_store(store.clone());
        }
        let task_gate: Arc<dyn TaskGate> = Arc::new(BusTaskGate {
            bus: self.bus.clone(),
            agent_id: self.id.clone(),
            session_id: session_id.clone(),
            kernel: loop_config.clone(),
            request: background_request,
            ports: background_ports,
            tasks: self.background_tasks.clone(),
        });
        let mut ports = AgentExecutionPorts::new(
            self.model_provider.clone(),
            tool_context,
            invocation_gateway,
            invocation_snapshot,
        )
        .with_ask_user_gate(ask_user_gate)
        .with_plan_gate(plan_gate)
        .with_task_gate(task_gate);
        if let Some(gate) = approval_gate {
            ports = ports.with_approval_gate(gate);
        }
        if let Some(store) = artifact_store {
            ports = ports.with_artifact_store(store);
        }

        // 3. Run loop with streaming
        let mut stream = Box::pin(agent_loop::run_stream(&loop_config, request, ports));
        tokio::pin!(interrupted);
        let mut final_message: Option<ModelResponse> = None;

        loop {
            let event = tokio::select! {
                biased;
                _ = &mut interrupted => {
                    self.cancel_pending_decisions(&session_id).await;
                    if let Some(store) = &self.session_store {
                        store
                            .finish_turn(
                                &session_id,
                                turn_id,
                                TurnState::Interrupted,
                                None,
                            )
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::FinishTurn,
                                    source,
                                )
                            })?;
                        self.observability
                            .record(RuntimeEvent::PersistenceFinished {
                                turn_id: turn_id.to_owned(),
                                session_id: session_id.clone(),
                                operation: RuntimePersistenceOperation::FinishTurn,
                                succeeded: true,
                            });
                    }
                    self.observability.record(RuntimeEvent::TurnInterrupted {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                    });
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::TurnInterrupted {
                            reason: "interrupted by user".into(),
                        },
                    ).await;
                    return Ok(());
                }
                event = stream.next() => event,
            };
            let Some(event) = event else {
                break;
            };
            match event {
                sylvander_agent::turn::event::AgentEvent::TextChunk(text) => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::TextDelta { delta: text },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ThinkingChunk(text) => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ThinkingDelta { delta: text },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ModelRetry {
                    attempt,
                    max_attempts,
                    delay_ms,
                    reason,
                    cause,
                } => {
                    self.observability.record(RuntimeEvent::ModelRetried {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                        attempt,
                    });
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ModelRetry {
                            attempt,
                            max_attempts,
                            delay_ms,
                            reason,
                            cause: public_retry_cause(cause),
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ToolCallStart { id, name, input } => {
                    self.observability.record(RuntimeEvent::ToolStarted {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                        tool_call_id: id.clone(),
                        tool_name: name.clone(),
                    });
                    if matches!(
                        name.as_str(),
                        "present_plan" | "update_plan" | "start_background_task"
                    ) {
                        continue;
                    }
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ToolCall {
                            call_id: id,
                            tool_name: name,
                            input,
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ToolCallOutputDelta {
                    id,
                    name,
                    delta,
                } => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ToolOutputDelta {
                            call_id: id,
                            tool_name: name,
                            delta,
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ToolTimedOut {
                    id,
                    name,
                    timeout_secs,
                } => {
                    self.observability.record(RuntimeEvent::ToolFinished {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                        tool_call_id: id.clone(),
                        tool_name: name,
                        succeeded: false,
                    });
                    publish_interaction_timeout(
                        &self.bus,
                        &session_id,
                        &self.id,
                        sylvander_api::InteractionTimeoutKind::Tool,
                        &id,
                        timeout_secs,
                        sylvander_api::TimeoutRecovery::NarrowScope,
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ToolFailureClassified {
                    id,
                    name,
                    kind,
                } => match kind {
                    sylvander_agent::tool::ToolFailureKind::FilesystemBoundaryPolicyViolation => {
                        self.observability
                            .record(RuntimeEvent::ToolFailureClassified {
                                turn_id: turn_id.to_owned(),
                                session_id: session_id.clone(),
                                tool_call_id: id,
                                tool_name: name,
                                kind: RuntimeToolFailureKind::FilesystemBoundaryPolicyViolation,
                            });
                    }
                    sylvander_agent::tool::ToolFailureKind::Unclassified => {}
                },
                sylvander_agent::turn::event::AgentEvent::ToolCallEnd {
                    id,
                    name,
                    output,
                    is_error,
                } => {
                    self.observability.record(RuntimeEvent::ToolFinished {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                        tool_call_id: id.clone(),
                        tool_name: name.clone(),
                        succeeded: !is_error,
                    });
                    if matches!(
                        name.as_str(),
                        "present_plan" | "update_plan" | "start_background_task"
                    ) {
                        continue;
                    }
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ToolResult {
                            call_id: id,
                            tool_name: name,
                            output,
                            is_error,
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ToolRejected { id, name, reason } => {
                    self.observability.record(RuntimeEvent::ToolFinished {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                        tool_call_id: id.clone(),
                        tool_name: name.clone(),
                        succeeded: false,
                    });
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ToolResult {
                            call_id: id,
                            tool_name: name,
                            output: reason,
                            is_error: true,
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::IterationStart { iteration } => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::IterationStart { iteration },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::IterationEnd {
                    iteration,
                    usage,
                    provider_usage,
                } => {
                    self.context_usage.write().await.insert(
                        session_id.clone(),
                        ContextUsage {
                            used: u32::try_from(provider_usage.total_input_tokens())
                                .unwrap_or(u32::MAX),
                            cache_read: u32::try_from(
                                provider_usage.cache_read_tokens.unwrap_or(0),
                            )
                            .unwrap_or(u32::MAX),
                            cache_write: u32::try_from(
                                provider_usage.cache_write_tokens.unwrap_or(0),
                            )
                            .unwrap_or(u32::MAX),
                        },
                    );
                    let mut input_tokens = usage.input_tokens;
                    let mut output_tokens = usage.output_tokens;
                    let iteration_cost = selected_pricing
                        .and_then(|pricing| usage_cost_nano_usd(pricing, &provider_usage));
                    let mut cost_nano_usd = iteration_cost;
                    if let Some(store) = &self.session_store {
                        let total = store
                            .record_usage(
                                &session_id,
                                u32::try_from(provider_usage.input_tokens).unwrap_or(u32::MAX),
                                u32::try_from(provider_usage.output_tokens).unwrap_or(u32::MAX),
                                iteration_cost,
                            )
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::RecordUsage,
                                    source,
                                )
                            })?;
                        input_tokens = total.input_tokens;
                        output_tokens = total.output_tokens;
                        cost_nano_usd = total.cost_nano_usd;
                        self.observability
                            .record(RuntimeEvent::PersistenceFinished {
                                turn_id: turn_id.to_owned(),
                                session_id: session_id.clone(),
                                operation: RuntimePersistenceOperation::RecordUsage,
                                succeeded: true,
                            });
                    }
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::IterationEnd {
                            iteration,
                            input_tokens: u32::try_from(input_tokens).unwrap_or(u32::MAX),
                            output_tokens: u32::try_from(output_tokens).unwrap_or(u32::MAX),
                            cost_nano_usd,
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::CompressionStarted => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::CompactionStarted { automatic: true },
                    )
                    .await;
                }
                // `BusAskUserGate` publishes the request when it installs the
                // pending answer. Forwarding the loop event too would stack
                // two identical TUI modals for one question.
                sylvander_agent::turn::event::AgentEvent::Compressed { .. }
                | sylvander_agent::turn::event::AgentEvent::AskUser { .. }
                | sylvander_agent::turn::event::AgentEvent::PlanProposed { .. }
                | sylvander_agent::turn::event::AgentEvent::PlanResolved { .. } => {}
                sylvander_agent::turn::event::AgentEvent::HistoryCompacted { layers, history } => {
                    if let Some(error) =
                        sylvander_agent::compress::layer::first_failure_error(&layers)
                    {
                        self.publish_stream(
                            &session_id,
                            sylvander_api::StreamEvent::CompactionFailed {
                                automatic: true,
                                reason: error.compatibility_reason().into(),
                            },
                        )
                        .await;
                    } else {
                        match self
                            .apply_compacted_history(&session_id, &history, &layers)
                            .await
                        {
                            Ok(()) => {
                                self.publish_stream(
                                    &session_id,
                                    sylvander_api::StreamEvent::CompactionCompleted {
                                        report: public_compaction_report(true, &layers),
                                    },
                                )
                                .await;
                            }
                            Err(error) => {
                                self.publish_stream(
                                    &session_id,
                                    sylvander_api::StreamEvent::CompactionFailed {
                                        automatic: true,
                                        reason: sylvander_agent::compress::error::CompactionError::new(
                                            sylvander_agent::compress::error::CompactionFailureCode::Persistence,
                                        )
                                        .compatibility_reason()
                                        .into(),
                                    },
                                )
                                .await;
                                return Err(error);
                            }
                        }
                    }
                }
                sylvander_agent::turn::event::AgentEvent::UserAnswer { call_id, answer } => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::UserAnswer { call_id, answer },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::Done(outcome) => {
                    final_message = Some(outcome.final_response);
                }
                sylvander_agent::turn::event::AgentEvent::Error(e) => {
                    return Err(AgentRunError::Loop(e));
                }
            }
        }

        // 4. Write final message, record the terminal fact, then publish Done.
        let msg = final_message.ok_or_else(|| {
            AgentRunError::Loop(AgentLoopError::Validation(
                "Agent event stream ended without a terminal outcome".into(),
            ))
        })?;
        let text = msg.text();
        if let Some(store) = &self.session_store {
            let user_id = self.sessions.read().await.get(&session_id).map_or_else(
                || "unix-client".into(),
                |context| context.metadata.user_id.clone(),
            );
            let caller =
                sylvander_api::SessionContext::new(user_id, self.id.clone(), session_id.clone());
            let message = ChatMessage::assistant(msg.content.clone());
            let content = serde_json::to_value(message).map_err(|_| {
                AgentRunError::session_persistence(
                    SessionPersistenceOperation::CompleteTurn,
                    SessionStoreError::Invalid("assistant message serialization failed".into()),
                )
            })?;
            store
                .complete_turn(
                    &caller,
                    TurnCompletion {
                        session_id: session_id.clone(),
                        turn_id: turn_id.to_owned(),
                        assistant_content: content,
                        model_id: msg.model.model.clone(),
                    },
                )
                .await
                .map_err(|source| {
                    AgentRunError::session_persistence(
                        SessionPersistenceOperation::CompleteTurn,
                        source,
                    )
                })?;
            self.observability
                .record(RuntimeEvent::PersistenceFinished {
                    turn_id: turn_id.to_owned(),
                    session_id: session_id.clone(),
                    operation: RuntimePersistenceOperation::CompleteTurn,
                    succeeded: true,
                });
        }
        let mut sessions = self.sessions.write().await;
        if let Some(ctx) = sessions.get_mut(&session_id) {
            ctx.append_assistant_message(msg);
        }
        drop(sessions);
        self.observability.record(RuntimeEvent::TurnCompleted {
            turn_id: turn_id.to_owned(),
            session_id: session_id.clone(),
        });
        self.publish_stream(&session_id, sylvander_api::StreamEvent::Done { text })
            .await;

        Ok(())
    }

    // -- helpers --

    async fn publish_stream(&self, session_id: &SessionId, event: sylvander_api::StreamEvent) {
        let msg = BusMessage::stream_event(session_id.clone(), self.id.clone(), event);
        let _ = self.bus.publish(msg).await;
    }

    async fn publish_error(&self, session_id: &SessionId, err: &AgentLoopError) {
        self.publish_stream(
            session_id,
            sylvander_api::StreamEvent::Error {
                message: err.to_string(),
            },
        )
        .await;
    }

    fn message_to_param(msg: &BusMessage) -> ChatMessage {
        if msg.attachments.is_empty() {
            return ChatMessage::user(&msg.payload);
        }
        let mut blocks = Vec::new();
        if !msg.payload.is_empty() {
            blocks.push(ContentBlock::Text {
                text: msg.payload.clone(),
            });
        }
        for attachment in &msg.attachments {
            match &attachment.content {
                sylvander_api::AttachmentContent::Text { text } => {
                    blocks.push(ContentBlock::Text {
                        text: format!(
                            "Attached {:?} `{}` ({}):\n{}",
                            attachment.kind, attachment.name, attachment.mime_type, text
                        ),
                    });
                }
                sylvander_api::AttachmentContent::Base64 { data } => {
                    if matches!(attachment.mime_type.as_str(), "image/png" | "image/jpeg") {
                        blocks.push(ContentBlock::Text {
                            text: format!("Attached image `{}`:", attachment.name),
                        });
                        blocks.push(ContentBlock::Image {
                            image: ImageContent {
                                source: MediaSource::Base64 {
                                    media_type: attachment.mime_type.clone(),
                                    data: data.clone(),
                                },
                                alt_text: Some(attachment.name.clone()),
                            },
                        });
                    }
                }
            }
        }
        ChatMessage::user_blocks(blocks)
    }
}

struct WorkspaceTurnContext {
    authoritative: Option<TurnContextCandidate>,
    retrieved: Vec<TurnContextCandidate>,
}

#[allow(clippy::too_many_arguments)]
async fn workspace_turn_context(
    agent_workspace: Option<&sylvander_api::SessionWorkspaceBinding>,
    task_workspace: Option<&sylvander_api::SessionWorkspaceBinding>,
    workspace_mounts: &[sylvander_api::SessionWorkspaceMount],
    fallback_task_workspace: &Path,
    execution_service: &RuntimeExecutionService,
    skill_features: &std::sync::RwLock<Vec<sylvander_api::PlatformFeature>>,
    query: &str,
    budget: sylvander_agent::turn_context::TurnContextBudget,
) -> Result<WorkspaceTurnContext, AgentRunError> {
    let agent_focus = agent_workspace
        .and_then(|binding| binding.instruction_focus.clone())
        .unwrap_or_default();
    let task_focus = task_workspace
        .and_then(|binding| binding.instruction_focus.clone())
        .unwrap_or_default();
    let agent_target = agent_workspace.map(workspace_target);
    let task_target = Some(task_workspace.map_or_else(
        || WorkspaceTarget::local(fallback_task_workspace, true),
        |binding| WorkspaceTarget {
            id: binding.execution_target.clone(),
            workspace_path: if binding.execution_target == "local" {
                fallback_task_workspace.to_path_buf()
            } else {
                binding.path.clone()
            },
            read_only: true,
        },
    ));
    let agent_executor = agent_target
        .as_ref()
        .map(|target| workspace_context_executor(execution_service, target))
        .transpose()?;
    let task_executor = task_target
        .as_ref()
        .map(|target| workspace_context_executor(execution_service, target))
        .transpose()?;
    let context = workspace_context::discover_with_report(
        agent_target
            .clone()
            .zip(agent_executor)
            .map(|(target, executor)| {
                workspace_context::WorkspaceContextSource::focused(
                    executor.as_ref(),
                    target,
                    agent_focus,
                )
            }),
        task_target
            .clone()
            .zip(task_executor)
            .map(|(target, executor)| {
                workspace_context::WorkspaceContextSource::focused(
                    executor.as_ref(),
                    target,
                    task_focus,
                )
            }),
    )
    .await
    .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
    *skill_features.write().unwrap() = context
        .skills
        .iter()
        .map(|skill| sylvander_api::PlatformFeature {
            kind: sylvander_api::PlatformFeatureKind::Skill,
            name: skill.name.clone(),
            status: match skill.status {
                workspace_context::SkillStatus::Active => {
                    sylvander_api::PlatformFeatureStatus::Active
                }
                workspace_context::SkillStatus::Disabled => {
                    sylvander_api::PlatformFeatureStatus::Configured
                }
                workspace_context::SkillStatus::Degraded => {
                    sylvander_api::PlatformFeatureStatus::Degraded
                }
            },
            summary: format!("prompt context only · {} ({})", skill.summary, skill.role),
            source: Some(format!("{}:{}", skill.target_id, skill.relative_path)),
            trust: Some(if skill.role == "agent-home" {
                sylvander_api::PlatformTrust::BuiltIn
            } else {
                sylvander_api::PlatformTrust::Workspace
            }),
            auth: sylvander_api::PlatformAuthStatus::NotRequired,
            capabilities: {
                let mut capabilities = skill.capabilities.clone();
                capabilities.push("prompt_context_only".into());
                capabilities
            },
            reloadable: true,
        })
        .collect();
    let mut prompt = context.prompt.unwrap_or_default();
    if !workspace_mounts.is_empty() {
        let mounts = workspace_mounts
            .iter()
            .map(|mount| {
                let mut operations = vec!["read"];
                if mount.capabilities.write {
                    operations.push("write");
                }
                if mount.capabilities.command {
                    operations.push("command");
                }
                if mount.capabilities.git {
                    operations.push("git");
                }
                let role = match mount.role {
                    sylvander_api::WorkspaceMountRole::AgentHome => "agent-home",
                    sylvander_api::WorkspaceMountRole::Task => "task",
                    sylvander_api::WorkspaceMountRole::Dependency => "dependency",
                    sylvander_api::WorkspaceMountRole::Artifact => "artifact",
                };
                format!("- @{} ({role}): {}", mount.reference, operations.join(", "))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !prompt.is_empty() {
            prompt.push_str("\n\n");
        }
        prompt.push_str(
            "# Available workspace mounts\n\
             Unqualified paths use the task workspace. Address another mount \
             as `@reference/path`; Command and Git accept `workspace: \"reference\"`.\n",
        );
        prompt.push_str(&mounts);
    }
    let authoritative = (!prompt.is_empty()).then(|| {
        TurnContextCandidate::authoritative(
            prompt,
            TurnContextProvenance::new(
                TurnContextSource::WorkspaceInstructions,
                task_target.as_ref().map_or_else(
                    || "workspace:unavailable".into(),
                    |target| format!("workspace:{}:instructions", target.id),
                ),
            ),
        )
    });
    let retrieved = match (task_target.as_ref(), task_executor) {
        (Some(target), Some(executor)) => {
            retrieve_workspace_context(executor.as_ref(), target, query, budget)
                .await
                .map_err(|error| AgentRunError::Configuration(error.to_string()))?
        }
        _ => Vec::new(),
    };
    Ok(WorkspaceTurnContext {
        authoritative,
        retrieved,
    })
}

fn workspace_target(binding: &sylvander_api::SessionWorkspaceBinding) -> WorkspaceTarget {
    WorkspaceTarget {
        id: binding.execution_target.clone(),
        workspace_path: binding.path.clone(),
        read_only: true,
    }
}

fn workspace_context_executor<'a>(
    execution_service: &'a RuntimeExecutionService,
    target: &WorkspaceTarget,
) -> Result<&'a Arc<dyn WorkspaceExecutor>, AgentRunError> {
    execution_service.resolve(&target.id).ok_or_else(|| {
        AgentRunError::Configuration(format!(
            "execution target `{}` is unavailable on this server",
            target.id
        ))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TurnCorrelation {
    turn: String,
    request: String,
    trace: String,
}

fn runtime_failure_kind(error: &AgentRunError) -> RuntimeFailureKind {
    match error {
        AgentRunError::UnknownSession(_) => RuntimeFailureKind::UnknownSession,
        AgentRunError::Authentication(_) => RuntimeFailureKind::Authentication,
        AgentRunError::Loop(_) => RuntimeFailureKind::AgentLoop,
        AgentRunError::Build(_) | AgentRunError::Configuration(_) => {
            RuntimeFailureKind::Configuration
        }
        AgentRunError::SessionPersistence { .. } => RuntimeFailureKind::Persistence,
    }
}

fn turn_failure_kind(error: &AgentRunError) -> TurnFailureKind {
    match error {
        AgentRunError::UnknownSession(_) => TurnFailureKind::UnknownSession,
        AgentRunError::Authentication(_) => TurnFailureKind::Authentication,
        AgentRunError::Loop(_) => TurnFailureKind::AgentLoop,
        AgentRunError::Build(_) | AgentRunError::Configuration(_) => TurnFailureKind::Configuration,
        AgentRunError::SessionPersistence { .. } => TurnFailureKind::Persistence,
    }
}

fn runtime_persistence_operation(
    operation: SessionPersistenceOperation,
) -> RuntimePersistenceOperation {
    match operation {
        SessionPersistenceOperation::InspectSession => RuntimePersistenceOperation::InspectSession,
        SessionPersistenceOperation::CreateSession => RuntimePersistenceOperation::CreateSession,
        SessionPersistenceOperation::RestoreHistory => RuntimePersistenceOperation::RestoreHistory,
        SessionPersistenceOperation::BeginTurn => RuntimePersistenceOperation::BeginTurn,
        SessionPersistenceOperation::RecordUsage => RuntimePersistenceOperation::RecordUsage,
        SessionPersistenceOperation::CompleteTurn => RuntimePersistenceOperation::CompleteTurn,
        SessionPersistenceOperation::FinishTurn => RuntimePersistenceOperation::FinishTurn,
        SessionPersistenceOperation::ReplaceHistory => RuntimePersistenceOperation::ReplaceHistory,
    }
}

impl TurnCorrelation {
    fn new(message: &BusMessage, turn_id: uuid::Uuid) -> Self {
        let turn_id = turn_id.to_string();
        Self {
            request: message.id.0.to_string(),
            trace: turn_id.clone(),
            turn: turn_id,
        }
    }
}

#[derive(Clone, Copy)]
struct ToolSessionExecution<'a> {
    metadata: &'a SessionMetadata,
    effective_config: Option<&'a sylvander_api::SessionEffectiveConfig>,
    execution_service: &'a RuntimeExecutionService,
}

fn tool_context_for_permissions(
    execution: ToolSessionExecution<'_>,
    agent_id: &AgentId,
    session_id: &SessionId,
    permissions: &sylvander_api::PermissionProfile,
    trusted_memory: bool,
    workspace_journal: Option<Arc<WorkspaceJournal>>,
    turn_id: Option<&str>,
) -> ToolContext {
    let metadata = execution.metadata;
    let binding = execution.effective_config.and_then(|config| {
        select_workspace_binding(
            config.user_workspace.as_ref(),
            config.agent_workspace.as_ref(),
        )
    });
    let target_id = binding.map_or("local", |binding| binding.execution_target.as_str());
    let workspace = binding.map_or(metadata.workspace.as_path(), |binding| {
        binding.path.as_path()
    });
    let permission_read_only = permissions.file_access != sylvander_api::FileAccess::WorkspaceWrite;
    let read_only = permission_read_only || binding.is_some_and(|binding| binding.read_only);
    let mut agent_execution = AgentExecutionContext::restricted_for(
        metadata.user_id.clone(),
        agent_id.0.clone(),
        session_id.0.clone(),
    )
    .with_started_at_unix_secs(now_secs())
    .with_workspace(ExecutionWorkspace {
        workspace_id: "primary".into(),
        target_id: target_id.to_owned(),
        read_only,
    });
    if let Some(turn_id) = turn_id {
        agent_execution = agent_execution.with_trace_id(turn_id);
    }
    let mut context = if trusted_memory {
        ToolContext::for_runtime(agent_execution)
    } else {
        ToolContext::new(agent_execution)
    };
    let target = WorkspaceTarget {
        id: target_id.to_owned(),
        workspace_path: workspace.to_path_buf(),
        read_only,
    };
    let executor = execution
        .effective_config
        .filter(|config| !config.workspace_mounts.is_empty())
        .map_or_else(
            || {
                execution
                    .execution_service
                    .resolve_or_unavailable(target_id)
            },
            |config| {
                let default_reference = config
                    .workspace_mounts
                    .iter()
                    .find(|mount| mount.role == sylvander_api::WorkspaceMountRole::Task)
                    .or_else(|| config.workspace_mounts.first())
                    .map(|mount| mount.reference.clone())
                    .expect("non-empty workspace composition has a default mount");
                let mounts = config.workspace_mounts.iter().map(|mount| {
                    let mut capabilities = WorkspaceCapabilities {
                        read: mount.capabilities.read,
                        write: mount.capabilities.write,
                        command: mount.capabilities.command,
                        git: mount.capabilities.git,
                    };
                    if permission_read_only {
                        capabilities.write = false;
                        capabilities.command = false;
                    }
                    let executor = execution
                        .execution_service
                        .resolve_or_unavailable(&mount.binding.execution_target);
                    (
                        mount.reference.clone(),
                        MountedWorkspace {
                            executor,
                            target: WorkspaceTarget {
                                id: mount.binding.execution_target.clone(),
                                workspace_path: mount.binding.path.clone(),
                                read_only: permission_read_only || mount.binding.read_only,
                            },
                            capabilities,
                        },
                    )
                });
                WorkspaceRouter::new(default_reference, mounts).map_or_else(
                    |_| {
                        Arc::new(UnavailableExecutor::new("workspace-composition"))
                            as Arc<dyn WorkspaceExecutor>
                    },
                    |router| Arc::new(router) as Arc<dyn WorkspaceExecutor>,
                )
            },
        );
    context = context.with_executor(executor, target);
    if target_id == "local"
        && !read_only
        && let Some(journal) = workspace_journal
    {
        let journal: Arc<dyn WorkspaceMutationJournal> = journal;
        context = context.with_workspace_journal(journal);
    }
    match permissions.file_access {
        sylvander_api::FileAccess::None => {}
        sylvander_api::FileAccess::ReadOnly => {
            context = context.with_capability(Cap::Read).with_capability(Cap::Git);
        }
        sylvander_api::FileAccess::WorkspaceWrite => {
            context = context
                .with_capability(Cap::Read)
                .with_capability(Cap::Write)
                .with_capability(Cap::Spawn)
                .with_capability(Cap::Git);
        }
    }
    if permissions.network_access == sylvander_api::NetworkAccess::Allowed {
        context = context.with_capability(Cap::Network);
        context.surface.network = NetworkPolicy::All;
    }
    if trusted_memory {
        context = context
            .with_capability(Cap::MemoryRead)
            .with_capability(Cap::MemoryWrite);
    }
    context
}

fn select_workspace_binding<'a>(
    user_workspace: Option<&'a sylvander_api::SessionWorkspaceBinding>,
    agent_workspace: Option<&'a sylvander_api::SessionWorkspaceBinding>,
) -> Option<&'a sylvander_api::SessionWorkspaceBinding> {
    user_workspace.or(agent_workspace)
}

// ---------------------------------------------------------------------------
// AgentRunBuilder
// ---------------------------------------------------------------------------

/// Builder for [`AgentRun`].
pub struct AgentRunBuilder {
    spec: AgentSpec,
    router: Arc<dyn ModelProvider>,
    model: ModelInfo,
    bus: Option<Arc<dyn MessageBus>>,
    observability: RuntimeObservability,
    tool_overrides: Option<ToolRegistry>,
    compression_overrides: Option<sylvander_agent::compress::pipeline::CompressionPipeline>,
    memory: Option<Arc<dyn MemoryStore>>,
    session_store: Option<Arc<dyn SessionStore>>,
    available_provider_models: Vec<ModelInfo>,
    qualified_model_lifecycles:
        HashMap<sylvander_api::ModelSelection, sylvander_api::ModelLifecycle>,
    qualified_model_pricing: HashMap<sylvander_api::ModelSelection, sylvander_api::ModelPricing>,
    prompt_resolver: Option<Arc<PromptResolver>>,
    user_profile_provider: Option<Arc<dyn UserProfileProvider>>,
    curated_context_provider: Option<Arc<dyn CuratedContextProvider>>,
    turn_context_budgets: TurnContextBudgets,
    approval_enabled: bool,
    approval_rules: Vec<sylvander_agent::approval::ApprovalRule>,
    approval_store_path: Option<PathBuf>,
    workspace_journal_path: Option<PathBuf>,
    execution_service: Option<RuntimeExecutionService>,
    artifact_service: Option<RuntimeArtifactService>,
    invocation_gateway: Option<Arc<dyn sylvander_agent::tool::invocation::ToolInvocationGateway>>,
}

impl AgentRunBuilder {
    fn new_qualified_router(
        spec: AgentSpec,
        router: Arc<dyn ModelProvider>,
        model: ModelInfo,
    ) -> Self {
        Self {
            spec,
            router,
            model,
            bus: None,
            observability: RuntimeObservability::new(),
            tool_overrides: None,
            compression_overrides: None,
            memory: None,
            session_store: None,
            available_provider_models: Vec::new(),
            qualified_model_lifecycles: HashMap::new(),
            qualified_model_pricing: HashMap::new(),
            prompt_resolver: None,
            user_profile_provider: None,
            curated_context_provider: None,
            turn_context_budgets: TurnContextBudgets::default(),
            approval_enabled: false,
            approval_rules: Vec::new(),
            approval_store_path: None,
            workspace_journal_path: None,
            execution_service: Some(RuntimeExecutionService::standalone_local()),
            artifact_service: None,
            invocation_gateway: None,
        }
    }

    #[must_use]
    pub fn bus(mut self, bus: Arc<dyn MessageBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Inject the one Runtime-owned built-in lifecycle recorder.
    #[must_use]
    pub(crate) fn observability(mut self, observability: RuntimeObservability) -> Self {
        self.observability = observability;
        self
    }

    #[must_use]
    pub fn memory(mut self, store: Arc<dyn MemoryStore>) -> Self {
        self.memory = Some(store);
        self
    }

    #[must_use]
    pub fn session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Attach Runtime's encrypted artifact service for every admitted turn.
    #[must_use]
    pub(crate) fn artifact_service(mut self, service: RuntimeArtifactService) -> Self {
        self.artifact_service = Some(service);
        self
    }

    #[must_use]
    pub fn override_tools(mut self, tools: ToolRegistry) -> Self {
        self.tool_overrides = Some(tools);
        self
    }

    /// Inject the Runtime-owned authorization and durable audit boundary used
    /// by every ordinary tool invocation.
    #[must_use]
    pub fn invocation_gateway(
        mut self,
        gateway: Arc<dyn sylvander_agent::tool::invocation::ToolInvocationGateway>,
    ) -> Self {
        self.invocation_gateway = Some(gateway);
        self
    }

    /// Register exact provider-qualified models. The configured provider
    /// adapter can route only entries belonging to its own provider.
    #[must_use]
    pub fn available_provider_models(mut self, models: Vec<ModelInfo>) -> Self {
        self.available_provider_models = models;
        self
    }

    /// Attach lifecycle truth to exact provider-qualified models.
    #[must_use]
    pub fn qualified_model_lifecycles(
        mut self,
        lifecycles: HashMap<sylvander_api::ModelSelection, sylvander_api::ModelLifecycle>,
    ) -> Self {
        self.qualified_model_lifecycles = lifecycles;
        self
    }

    /// Attach pricing snapshots to exact provider-qualified models.
    #[must_use]
    pub fn qualified_model_pricing(
        mut self,
        pricing: HashMap<sylvander_api::ModelSelection, sylvander_api::ModelPricing>,
    ) -> Self {
        self.qualified_model_pricing = pricing;
        self
    }

    /// Attach the same immutable prompt resolver used by session composition.
    #[must_use]
    pub fn prompt_resolver(mut self, resolver: Arc<PromptResolver>) -> Self {
        self.prompt_resolver = Some(resolver);
        self
    }

    /// Inject Runtime-owned live profile lookup for each authenticated turn.
    #[must_use]
    pub fn user_profile_provider(mut self, provider: Arc<dyn UserProfileProvider>) -> Self {
        self.user_profile_provider = Some(provider);
        self
    }

    /// Inject committed Guardian output for bounded typed turn retrieval.
    #[must_use]
    pub fn curated_context_provider(mut self, provider: Arc<dyn CuratedContextProvider>) -> Self {
        self.curated_context_provider = Some(provider);
        self
    }

    /// Replace the immutable per-layer context limits for every turn.
    #[must_use]
    pub fn turn_context_budgets(mut self, budgets: TurnContextBudgets) -> Self {
        self.turn_context_budgets = budgets;
        self
    }

    /// Enable bus-based tool approval (opt-in).
    #[must_use]
    pub fn enable_approval(mut self) -> Self {
        self.approval_enabled = true;
        self
    }

    /// Set static approval rules. Auto-approve/auto-reject matching tools
    /// before falling back to bus approval.
    #[must_use]
    pub fn approval_rules(mut self, rules: Vec<sylvander_agent::approval::ApprovalRule>) -> Self {
        self.approval_enabled = true; // rules imply approval
        self.approval_rules = rules;
        self
    }

    /// Enable durable exact-request approvals. Without this explicit store,
    /// the Agent advertises only one-shot and session scopes.
    #[must_use]
    pub fn approval_store(mut self, path: impl Into<PathBuf>) -> Self {
        self.approval_enabled = true;
        self.approval_store_path = Some(path.into());
        self
    }

    #[must_use]
    pub fn workspace_journal(mut self, path: impl Into<PathBuf>) -> Self {
        self.workspace_journal_path = Some(path.into());
        self
    }

    /// Inject the immutable execution environments selected by Runtime.
    #[must_use]
    pub(crate) fn execution_service(mut self, service: RuntimeExecutionService) -> Self {
        self.execution_service = Some(service);
        self
    }

    pub fn override_compression(
        mut self,
        pipeline: sylvander_agent::compress::pipeline::CompressionPipeline,
    ) -> Self {
        self.compression_overrides = Some(pipeline);
        self
    }

    /// Build the [`AgentRun`] without exposing its session issuer.
    pub fn build(self) -> Result<AgentRun, AgentRunError> {
        self.build_with_session_issuer().map(|(run, _)| run)
    }

    /// Build a run and return the runtime-owned issuer for authenticated
    /// session admission. Keep the issuer at the trusted service boundary.
    pub fn build_with_session_issuer(
        self,
    ) -> Result<(AgentRun, AgentSessionIssuer), AgentRunError> {
        let execution_service = self
            .execution_service
            .ok_or_else(|| AgentRunError::Build("Runtime execution service is required".into()))?;
        let id = self.spec.id.clone();
        let bus = self
            .bus
            .ok_or_else(|| AgentRunError::Build("bus is required".into()))?;

        let approval_memory =
            ApprovalMemory::load(self.approval_store_path.clone()).map_err(AgentRunError::Build)?;
        let (memory, memory_source) = match self.memory {
            Some(store) => (Some(store), MemorySource::RuntimeInjected),
            None => (None, MemorySource::None),
        };

        if self.model.reference.provider != self.spec.model.provider
            || self.model.reference.model != self.spec.model.model_name
        {
            return Err(AgentRunError::Build(
                "provider model does not match the Agent specification".into(),
            ));
        }
        let model_info = self.model.clone();
        let primary_selection = sylvander_api::ModelSelection {
            provider_id: self.model.reference.provider.clone(),
            model_id: self.model.reference.model.clone(),
        };
        let mut catalog = self
            .available_provider_models
            .iter()
            .map(|exact| {
                (
                    sylvander_api::ModelSelection {
                        provider_id: exact.reference.provider.clone(),
                        model_id: exact.reference.model.clone(),
                    },
                    exact.clone(),
                    Some(exact.clone()),
                )
            })
            .collect::<Vec<_>>();
        catalog.push((
            primary_selection.clone(),
            model_info.clone(),
            Some(self.model.clone()),
        ));
        let available_models = catalog
            .into_iter()
            .map(|(selection, shadow, exact)| {
                let model = RuntimeModel {
                    lifecycle: self
                        .qualified_model_lifecycles
                        .get(&selection)
                        .cloned()
                        .unwrap_or_default(),
                    pricing: self.qualified_model_pricing.get(&selection).copied(),
                    selection: selection.clone(),
                    shadow,
                    exact,
                };
                (selection, model)
            })
            .collect();
        let runtime_models = RuntimeModels {
            available: available_models,
            current: primary_selection,
            reasoning_effort: sylvander_api::ReasoningEffort::Off,
        };
        let runtime_permissions = sylvander_api::PermissionProfile {
            file_access: sylvander_api::FileAccess::WorkspaceWrite,
            network_access: sylvander_api::NetworkAccess::Denied,
            approval_policy: if self.approval_enabled {
                sylvander_api::ApprovalPolicy::Ask
            } else {
                sylvander_api::ApprovalPolicy::Allow
            },
        };

        let mut loop_builder = AgentLoop::builder()
            .max_iterations(self.spec.behavior.max_iterations)
            .max_retries(self.spec.behavior.max_retries);
        if let Some(pipeline) = self.compression_overrides {
            loop_builder = loop_builder.compression_pipeline(pipeline);
        }
        let loop_config = loop_builder.build();
        let system_prompt = (!self.spec.persona.system_prompt.is_empty())
            .then(|| self.spec.persona.system_prompt.clone());
        let tools = self.tool_overrides.unwrap_or_default();
        let invocation_gateway = self.invocation_gateway.unwrap_or_else(|| {
            sylvander_agent::tool::invocation::RegistryBoundToolGateway::new(
                tools.invocation_descriptors(),
            )
        });

        let workspace_journal = self
            .workspace_journal_path
            .map(|path| Arc::new(WorkspaceJournal::new(path)));
        let session_authority = Arc::new(SessionAuthorityMarker);
        let issuer = AgentSessionIssuer {
            authority: session_authority.clone(),
        };
        let run = AgentRun {
            inner: Arc::new(AgentRunInner {
                id,
                spec: self.spec,
                loop_config,
                model_provider: self.router,
                system_prompt,
                tools,
                invocation_gateway,
                session_tool_surfaces: RwLock::new(HashMap::new()),
                runtime_models: RwLock::new(runtime_models),
                runtime_permissions: RwLock::new(runtime_permissions),
                prompt_resolver: self.prompt_resolver,
                user_profile_provider: self.user_profile_provider,
                curated_context_provider: self.curated_context_provider,
                turn_context_budgets: self.turn_context_budgets,
                turn_context_manifests: RwLock::new(HashMap::new()),
                context_usage: RwLock::new(HashMap::new()),
                workspace_journal,
                execution_service,
                artifact_service: self.artifact_service,
                skill_features: std::sync::RwLock::new(Vec::new()),
                bus,
                observability: self.observability,
                sessions: RwLock::new(HashMap::new()),
                authenticated_sessions: RwLock::new(HashSet::new()),
                authenticated_session_authority_active: AtomicBool::new(false),
                session_authority,
                session_store: self.session_store,
                memory,
                memory_source,
                approval_enabled: self.approval_enabled,
                approval_rules: self.approval_rules,
                pending_approvals: Arc::new(Mutex::new(HashMap::new())),
                approval_memory: Arc::new(Mutex::new(approval_memory)),
                pending_answers: Arc::new(Mutex::new(HashMap::new())),
                pending_plans: Arc::new(Mutex::new(HashMap::new())),
                background_tasks: Arc::new(Mutex::new(HashMap::new())),
                session_locks: Mutex::new(HashMap::new()),
                active_turns: Mutex::new(HashMap::new()),
            }),
        };
        Ok((run, issuer))
    }
}

// ---------------------------------------------------------------------------
// AgentRunError
// ---------------------------------------------------------------------------

fn prompt_integrity_error() -> AgentRunError {
    AgentRunError::Configuration("prompt integrity verification failed".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPersistenceOperation {
    InspectSession,
    CreateSession,
    RestoreHistory,
    BeginTurn,
    RecordUsage,
    CompleteTurn,
    FinishTurn,
    ReplaceHistory,
}

impl std::fmt::Display for SessionPersistenceOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InspectSession => "inspect_session",
            Self::CreateSession => "create_session",
            Self::RestoreHistory => "restore_history",
            Self::BeginTurn => "begin_turn",
            Self::RecordUsage => "record_usage",
            Self::CompleteTurn => "complete_turn",
            Self::FinishTurn => "finish_turn",
            Self::ReplaceHistory => "replace_history",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    #[error("unknown session: {0}")]
    UnknownSession(SessionId),
    #[error("session authentication error: {0}")]
    Authentication(String),
    #[error("loop error: {0}")]
    Loop(#[from] AgentLoopError),
    #[error("build error: {0}")]
    Build(String),
    #[error("session configuration error: {0}")]
    Configuration(String),
    #[error("session persistence failed during {operation}")]
    SessionPersistence {
        operation: SessionPersistenceOperation,
        #[source]
        source: SessionStoreError,
    },
}

impl AgentRunError {
    fn session_persistence(
        operation: SessionPersistenceOperation,
        source: SessionStoreError,
    ) -> Self {
        Self::SessionPersistence { operation, source }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../../tests/unit/agent_run.rs"]
mod tests;
