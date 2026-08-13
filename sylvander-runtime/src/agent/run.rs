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
//! loop pauses (via [`sylvander_agent::approval::ApprovalGate`]) and the engine processes
//! `ApproveTool` responses concurrently via spawned `handle_message`
//! tasks. Per-session locks prevent concurrent execution on the same
//! session.

use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tracing::{info, warn};

use sylvander_api::{
    AgentInstanceId, PlatformAuthStatus, PlatformFeature, PlatformFeatureKind,
    PlatformFeatureStatus, PlatformTrust,
};
use sylvander_llm_core::{
    CacheHint, ChatMessage, ChatRole, ModelCapabilities, ModelInfo, ModelProvider,
    SystemInstruction, TokenUsage,
};

#[cfg(test)]
use crate::agent::approval::ApprovalGrantContext;
use crate::agent::approval::ApprovalMemory;
use crate::agent_definition::{AgentId, AgentSpec, SessionId};
use crate::execution::RuntimeExecutionService;
use crate::observability::RuntimeObservability;
use crate::prompt_contract::{agent_model_selection, public_prompt_manifest};
use crate::session::{AgentSessionKey, SessionContext, SessionMetadata, now_secs};
use crate::storage::artifact::RuntimeArtifactService;
#[cfg(test)]
use crate::storage::session::TurnStart;
use crate::storage::session::{
    MessageRole as StoredMessageRole, ReplacementMessage, SessionLifetime, SessionStore,
    SessionStoreError, StoredSession,
};
use crate::storage::workspace_journal::{RollbackPreview, RollbackReport, WorkspaceJournal};
#[cfg(test)]
use sylvander_agent::approval::{ApprovalDecision, ApprovalGate};
#[cfg(test)]
use sylvander_agent::ask_user_gate::AskUserGate;
use sylvander_agent::compress::error::{CompactionError, CompactionFailureCode};
use sylvander_agent::compress::layer::CompressionLayer;
use sylvander_agent::kernel::agent_loop::AgentLoop;
use sylvander_agent::memory::curated::CuratedContextProvider;
use sylvander_agent::memory::store::{
    MemoryAppend, MemoryEntry, MemoryExecutionContext, MemoryFilter, MemoryStore, MemoryStoreError,
};
#[cfg(test)]
use sylvander_agent::plan_gate::{PlanDecision, PlanGate};
use sylvander_agent::prompt::PromptResolver;
use sylvander_agent::tool::invocation::{
    CapabilityFeatureKind, ToolInvocationDescriptor, ToolInvocationGateway,
};
use sylvander_agent::tool::{RegisteredTool, ToolRegistry};
#[cfg(test)]
use sylvander_agent::tool_context::{Cap, ToolContext};
use sylvander_agent::tools::MemoryReadTool;
use sylvander_agent::turn::execution_context::AgentExecutionContext;
use sylvander_agent::turn::identity::{
    AgentId as KernelAgentId, SessionId as KernelSessionId, UserId as KernelUserId,
};
use sylvander_agent::turn::machine::TurnSnapshot;
use sylvander_agent::turn_context::{TurnContextBudgets, TurnContextManifest};
use sylvander_agent::user_profile_prompt::{UserProfilePromptLayer, compose_user_profile_prompt};
use sylvander_agent::user_profile_provider::{UserProfileProvider, UserProfileSubject};
#[cfg(test)]
use sylvander_agent::workspace_executor::{WorkspaceExecutor, WorkspaceTarget};
use sylvander_api::{
    AgentStatus as BusAgentStatus, BusMessage, MessageKind, Recipient, SystemMessage,
};
use sylvander_channel::{MessageBus, SubscriptionFilter};

mod background;
mod builder;
mod error;
mod interaction;
mod orchestration;
mod projection;
pub(crate) mod recovery;
#[path = "workspace_context.rs"]
mod workspace_context;

use background::ActiveBackgroundTask;
pub use builder::AgentRunBuilder;
use error::prompt_integrity_error;
pub use error::{AgentRunError, SessionPersistenceOperation};
#[cfg(test)]
use interaction::{BusApprovalGate, BusAskUserGate, BusPlanGate};
use interaction::{
    InteractionKey, PendingAnswer, PendingApproval, PendingPlan, normalize_rejection_reason,
};

fn interaction_key(message: &BusMessage, subject_id: &str) -> Option<InteractionKey> {
    if let Recipient::AgentInstance { instance_id, .. } = &message.recipient {
        return Some(InteractionKey::new(
            instance_id.clone(),
            message.session_id.clone(),
            subject_id,
        ));
    }
    None
}
#[cfg(test)]
use orchestration::{
    ToolSessionExecution, TurnCorrelation, select_workspace_binding, tool_context_for_permissions,
    workspace_turn_context,
};
#[cfg(test)]
use projection::usage_cost_nano_usd;
use projection::{
    agent_plan_decision, compaction_summary, public_capability_names, public_compaction_report,
    public_tool_feature,
};

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
    sessions: RwLock<HashMap<AgentSessionKey, SessionContext>>,
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
    pending_approvals: Arc<Mutex<HashMap<InteractionKey, PendingApproval>>>,
    /// Agent-owned approval memory. Session grants are isolated by session;
    /// persistent grants exist only when the operator configured a store.
    approval_memory: Arc<Mutex<ApprovalMemory>>,
    /// Pending `AskUser` answers (shared with `BusAskUserGate`).
    pending_answers: Arc<Mutex<HashMap<InteractionKey, PendingAnswer>>>,
    /// Pending typed plan decisions (shared with `BusPlanGate`).
    pending_plans: Arc<Mutex<HashMap<InteractionKey, PendingPlan>>>,
    /// Independently cancellable read-only background runs.
    background_tasks: Arc<Mutex<HashMap<String, ActiveBackgroundTask>>>,
    /// Per-session concurrency locks (M12).
    session_locks: Mutex<HashMap<SessionId, Arc<Mutex<()>>>>,
    /// One cancellation sender per session that currently owns its execution
    /// lock. Queued turns do not replace the active sender.
    active_turns: Mutex<HashMap<SessionId, ActiveTurn>>,
    /// Latest typed Agent-machine state for each currently executing turn.
    turn_snapshots: RwLock<HashMap<SessionId, RuntimeTurnSnapshot>>,
}

fn sole_session_context<'a>(
    sessions: &'a HashMap<AgentSessionKey, SessionContext>,
    session_id: &SessionId,
) -> Option<&'a SessionContext> {
    let mut matches = sessions
        .iter()
        .filter(|(key, _)| &key.session_id == session_id)
        .map(|(_, context)| context);
    let context = matches.next()?;
    matches.next().is_none().then_some(context)
}

fn sole_session_context_mut<'a>(
    sessions: &'a mut HashMap<AgentSessionKey, SessionContext>,
    session_id: &SessionId,
) -> Option<&'a mut SessionContext> {
    let key = {
        let mut matches = sessions.keys().filter(|key| &key.session_id == session_id);
        let key = matches.next()?.clone();
        if matches.next().is_some() {
            return None;
        }
        key
    };
    sessions.get_mut(&key)
}

fn remove_session_contexts(
    sessions: &mut HashMap<AgentSessionKey, SessionContext>,
    session_id: &SessionId,
) {
    sessions.retain(|key, _| &key.session_id != session_id);
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
    if expected.iter().any(|descriptor| {
        !actual.authorizes(
            &descriptor.name,
            descriptor.class,
            descriptor.recovery_policy,
        )
    }) || actual
        .features()
        .iter()
        .filter(|feature| matches!(feature.kind, CapabilityFeatureKind::Executable(..)))
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
            current: self.current.clone(),
            reasoning_effort: self.reasoning_effort,
            models,
        }
    }
}

/// A running agent instance — cheap `Clone` handle.
#[derive(Clone)]
pub struct AgentRun {
    pub(crate) inner: Arc<AgentRunInner>,
}

/// Content-free current state of one actively executing Runtime turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTurnSnapshot {
    pub turn_id: String,
    pub state: TurnSnapshot,
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
    pub(crate) async fn replay_classified_tool_calls(
        &self,
        session_id: &SessionId,
        recovery_owner: &str,
        observed_at: i64,
    ) -> Result<u64, AgentRunError> {
        self.inner
            .replay_classified_tool_calls(session_id, recovery_owner, observed_at)
            .await
    }

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
            Some(session_id) => {
                let sessions = self.inner.sessions.read().await;
                sole_session_context(&sessions, session_id).map_or(0, SessionContext::len)
            }
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
        let mut history = {
            let sessions = self.inner.sessions.read().await;
            sole_session_context(&sessions, session_id)
                .ok_or_else(|| CompactionError::new(CompactionFailureCode::SessionUnavailable))?
                .history_snapshot()
        };
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
        if sole_session_context(&*self.inner.sessions.read().await, session_id).is_none() {
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
        let key = ctx.key();
        self.inner.sessions.write().await.insert(key, ctx);
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
        remove_session_contexts(&mut *self.inner.sessions.write().await, session_id);
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
        let mut sessions = self
            .inner
            .sessions
            .read()
            .await
            .keys()
            .map(|key| key.session_id.clone())
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.0.cmp(&right.0));
        sessions.dedup();
        sessions
    }

    /// Get a session context.
    pub async fn get_session(&self, session_id: &SessionId) -> Option<SessionContext> {
        sole_session_context(&*self.inner.sessions.read().await, session_id).cloned()
    }

    /// Return the latest Agent-machine state while this Session owns a turn.
    pub async fn active_turn_snapshot(
        &self,
        session_id: &SessionId,
    ) -> Option<RuntimeTurnSnapshot> {
        self.inner
            .turn_snapshots
            .read()
            .await
            .get(session_id)
            .cloned()
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
                                        .insert(context.key(), context);
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
                            remove_session_contexts(
                                &mut *self.inner.sessions.write().await,
                                session_id,
                            );
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
                            let request = if let Some(key) = interaction_key(&msg, call_id) {
                                self.inner.pending_approvals.lock().await.remove(&key)
                            } else {
                                warn!(
                                    agent_id = %self.inner.id,
                                    session_id = %msg.session_id,
                                    %call_id,
                                    "ignored approval response without agent instance recipient"
                                );
                                None
                            };
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
                                interaction_key(&msg, call_id).and_then(|key| pending.remove(&key))
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
                                interaction_key(&msg, plan_id).and_then(|key| pending.remove(&key))
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
                        if sole_session_context(&sessions, &sid).is_none() {
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
        let session =
            sole_session_context(&sessions, session_id).ok_or(MemoryStoreError::AccessDenied)?;
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
        let (metadata, agent_instance_id) = {
            let sessions = self.sessions.read().await;
            let Some(session) = sole_session_context(&sessions, session_id) else {
                return Err(AgentRunError::UnknownSession(session_id.clone()));
            };
            (session.metadata.clone(), session.agent_instance_id.clone())
        };
        if compaction_summary(layers).is_some()
            && let Some(store) = &self.session_store
        {
            let caller = sylvander_api::SessionContext::new(
                metadata.user_id,
                self.id.clone(),
                session_id.clone(),
            )
            .with_agent_instance(agent_instance_id);
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
        let session = sole_session_context_mut(&mut sessions, session_id)
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
        let Some(store) = &self.session_store else {
            return Ok(SessionContext::new(
                session_id.clone(),
                AgentInstanceId::new(format!("moderator:{}", session_id.0)),
                metadata.clone(),
            ));
        };

        let mut stored = match store.get(session_id).await {
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
                stored
            }
            Ok(Some(stored)) => stored,
            Err(source) => {
                return Err(AgentRunError::session_persistence(
                    SessionPersistenceOperation::InspectSession,
                    source,
                ));
            }
        };

        if stored.effective_config.is_none() {
            stored.effective_config = Some(self.direct_session_config(&stored.metadata).await);
            store.save(&stored).await.map_err(|source| {
                AgentRunError::session_persistence(
                    SessionPersistenceOperation::RestoreMembership,
                    source,
                )
            })?;
        }

        let membership = if let Some(membership) = store
            .session_membership(session_id)
            .await
            .map_err(|source| {
                AgentRunError::session_persistence(
                    SessionPersistenceOperation::RestoreMembership,
                    source,
                )
            })? {
            membership
        } else {
            let effective = stored.effective_config.as_ref().ok_or_else(|| {
                AgentRunError::Configuration(
                    "persistent Session has no effective Agent configuration".into(),
                )
            })?;
            let membership = crate::runtime::initial_session_membership(&stored, effective)
                .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
            store
                .save_session_membership(&membership, None)
                .await
                .map_err(|source| {
                    AgentRunError::session_persistence(
                        SessionPersistenceOperation::RestoreMembership,
                        source,
                    )
                })?;
            membership
        };
        let moderator_instance_id = membership.governance.moderator_instance_id;
        let mut context = SessionContext::new(
            session_id.clone(),
            moderator_instance_id.clone(),
            stored.metadata.clone(),
        );

        let caller = sylvander_api::SessionContext::new(
            stored.metadata.user_id.clone(),
            self.id.clone(),
            session_id.clone(),
        )
        .with_agent_instance(moderator_instance_id);
        let mut messages = store
            .read_history(&caller, session_id, false, None)
            .await
            .map_err(|source| {
                AgentRunError::session_persistence(
                    SessionPersistenceOperation::RestoreHistory,
                    source,
                )
            })?;
        if messages.is_empty() {
            let legacy_caller = sylvander_api::SessionContext::new(
                stored.metadata.user_id.clone(),
                self.id.clone(),
                session_id.clone(),
            );
            messages = store
                .read_history(&legacy_caller, session_id, false, None)
                .await
                .map_err(|source| {
                    AgentRunError::session_persistence(
                        SessionPersistenceOperation::RestoreHistory,
                        source,
                    )
                })?;
        }
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
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../../tests/unit/agent_run.rs"]
mod tests;
