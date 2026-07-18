//! Agent runtime — the bridge between `AgentLoop` and the outside world.
//!
//! [`AgentRun`](crate::run::AgentRun) is a running agent instance. It is a cheap `Clone` handle
//! to shared state (`AgentRunInner`).
//!
//! # Memory: mechanism first, tools second
//!
//! Memory is agent infrastructure. The *read* path is exposed as a tool
//! so the model can autonomously retrieve context. The *write* path is
//! system-driven via [`AgentRun::remember`](crate::run::AgentRun::remember).
//!
//! # Session: engineering layer, model-invisible
//!
//! Sessions are purely for message routing and context isolation. The
//! model never sees session IDs.
//!
//! # Approval (M12)
//!
//! Tool approval flows through the bus. When approval is needed, the
//! loop pauses (via [`ApprovalGate`](crate::approval::ApprovalGate)) and the engine processes
//! `ApproveTool` responses concurrently via spawned `handle_message`
//! tasks. Per-session locks prevent concurrent execution on the same
//! session.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tracing::{Instrument as _, info, warn};

use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use sylvander_llm_core::{ModelInfo as ProviderModelInfo, ModelProvider};

use crate::approval::{ApprovalBatchResult, ApprovalDecision, ApprovalGate, ToolUseRequest};
use crate::approval_store::{
    ApprovalGrantContext, ApprovalGrantKey, ApprovalMemory, approval_policy_revision,
};
use crate::ask_user_gate::AskUserGate;
use crate::bus::{
    AgentStatus as BusAgentStatus, BusMessage, MessageBus, MessageKind, Recipient, Sender,
    StreamEvent, SubscriptionFilter, SystemMessage, ToolCallInfo,
};
use crate::compress::layer::CompressionLayer;
use crate::error::AgentLoopError;
use crate::loop_::{self, AgentLoop};
use crate::plan_gate::PlanGate;
use crate::prompt::{PromptResolver, SHARED_SAFETY_PROMPT};
use crate::session::{SessionContext, SessionMetadata, now_secs};
use crate::session_store::{
    MessageRole as StoredMessageRole, ReplacementMessage, SessionLifetime, SessionStore,
    StoredSession, TurnStart,
};
use crate::spec::{AgentId, AgentSpec, SessionId};
use crate::task_gate::TaskGate;
use crate::tool::{Tool, ToolRegistry};
use crate::tool_context::{Cap, NetworkPolicy, ToolContext};
use crate::tools::MemoryReadTool;
use crate::tools::memory::{
    MemoryAppend, MemoryEntry, MemoryExecutionContext, MemoryFilter, MemoryStore, MemoryStoreError,
};
use crate::turn_context::{
    TurnContextBudgets, TurnContextCandidate, TurnContextInputs, TurnContextLayerKind,
    TurnContextManifest, TurnContextProvenance, TurnContextSource, compose_turn_context,
    retrieve_relationship_context, retrieve_workspace_context,
};
use crate::workspace_executor::{
    LocalExecutor, MountedWorkspace, UnavailableExecutor, WorkspaceExecutor, WorkspaceRouter,
    WorkspaceTarget,
};

#[path = "workspace_context.rs"]
mod workspace_context;
use crate::user_profile_prompt::{UserProfilePromptLayer, compose_user_profile_prompt};
use crate::user_profile_provider::{UserProfileProvider, UserProfileSubject};

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
    /// Mutable selection read once at the start of every turn. Active turns
    /// keep their cloned `AgentLoop` and are never mutated underneath.
    runtime_models: RwLock<RuntimeModels>,
    runtime_permissions: RwLock<sylvander_protocol::PermissionProfile>,
    prompt_resolver: Option<Arc<PromptResolver>>,
    user_profile_provider: Option<Arc<dyn UserProfileProvider>>,
    turn_context_budgets: TurnContextBudgets,
    turn_context_manifests: RwLock<HashMap<SessionId, TurnContextManifest>>,
    /// Last provider-confirmed prompt usage for each session. This is window
    /// occupancy, unlike the durable cumulative billing counters.
    context_usage: RwLock<HashMap<SessionId, ContextUsage>>,
    workspace_journal: Option<Arc<crate::workspace_journal::WorkspaceJournal>>,
    /// Server-owned executor adapters keyed by exact execution-target id.
    workspace_executors: HashMap<String, Arc<dyn WorkspaceExecutor>>,
    skill_features: std::sync::RwLock<Vec<sylvander_protocol::PlatformFeature>>,
    /// Handle to the message bus.
    bus: Arc<dyn MessageBus>,
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
    approval_rules: Vec<crate::approval::ApprovalRule>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum MemorySource {
    None,
    RuntimeInjected,
}

struct PendingApproval {
    session_id: SessionId,
    grant: ApprovalGrantKey,
    persistent_identity_authorized: bool,
    allowed_scopes: Vec<sylvander_protocol::ApprovalScope>,
    sender: oneshot::Sender<crate::approval::ApprovalDecision>,
}

struct PendingAnswer {
    session_id: SessionId,
    sender: oneshot::Sender<Vec<String>>,
}

struct PendingPlan {
    session_id: SessionId,
    sender: oneshot::Sender<crate::bus::PlanDecision>,
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
    selection: sylvander_protocol::ModelSelection,
    shadow: ModelInfo,
    exact: Option<ProviderModelInfo>,
    lifecycle: sylvander_protocol::ModelLifecycle,
    pricing: Option<sylvander_protocol::ModelPricing>,
}

struct RuntimeModels {
    available: HashMap<sylvander_protocol::ModelSelection, RuntimeModel>,
    current: sylvander_protocol::ModelSelection,
    reasoning_effort: sylvander_protocol::ReasoningEffort,
}

#[derive(Debug, Clone, Copy, Default)]
struct ContextUsage {
    used: u32,
    cache_read: u32,
    cache_write: u32,
}

impl RuntimeModels {
    fn resolve_legacy_id(
        &self,
        model_id: &str,
    ) -> Result<sylvander_protocol::ModelSelection, String> {
        let matches = self
            .available
            .keys()
            .filter(|selection| selection.model_id == model_id)
            .cloned()
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => Err(format!("model `{model_id}` is not available")),
            [selection] => Ok(selection.clone()),
            _ => Err(format!(
                "model `{model_id}` is ambiguous; select it with a provider id"
            )),
        }
    }

    fn public_info(&self) -> sylvander_protocol::RuntimeModelInfo {
        let mut models = self
            .available
            .values()
            .map(|model| {
                let reasoning_efforts = if model
                    .shadow
                    .capabilities
                    .contains(ModelCapabilities::EXTENDED_THINKING)
                {
                    vec![
                        sylvander_protocol::ReasoningEffort::Off,
                        sylvander_protocol::ReasoningEffort::Low,
                        sylvander_protocol::ReasoningEffort::Medium,
                        sylvander_protocol::ReasoningEffort::High,
                    ]
                } else {
                    vec![sylvander_protocol::ReasoningEffort::Off]
                };
                sylvander_protocol::ModelDescriptor {
                    id: model.selection.model_id.clone(),
                    provider: model.selection.provider_id.clone(),
                    capabilities: model.shadow.capabilities.bits(),
                    capability_names: public_capability_names(model.shadow.capabilities),
                    reasoning_efforts,
                    lifecycle: model.lifecycle.clone(),
                    pricing: model.pricing,
                }
            })
            .collect::<Vec<_>>();
        models.sort_by(|left, right| (&left.provider, &left.id).cmp(&(&right.provider, &right.id)));
        sylvander_protocol::RuntimeModelInfo {
            current_model: self.current.model_id.clone(),
            reasoning_effort: self.reasoning_effort,
            models,
        }
    }
}

fn public_capability_names(
    capabilities: ModelCapabilities,
) -> Vec<sylvander_protocol::ModelCapability> {
    [
        (
            ModelCapabilities::EXTENDED_THINKING,
            sylvander_protocol::ModelCapability::ExtendedThinking,
        ),
        (
            ModelCapabilities::PROMPT_CACHING,
            sylvander_protocol::ModelCapability::PromptCaching,
        ),
        (
            ModelCapabilities::STRUCTURED_OUTPUT,
            sylvander_protocol::ModelCapability::StructuredOutput,
        ),
        (
            ModelCapabilities::TOOL_USE,
            sylvander_protocol::ModelCapability::ToolUse,
        ),
        (
            ModelCapabilities::VISION,
            sylvander_protocol::ModelCapability::Vision,
        ),
        (
            ModelCapabilities::DOCUMENT_INPUT,
            sylvander_protocol::ModelCapability::DocumentInput,
        ),
    ]
    .into_iter()
    .filter_map(|(flag, name)| capabilities.contains(flag).then_some(name))
    .collect()
}

fn usage_cost_nano_usd(
    pricing: sylvander_protocol::ModelPricing,
    usage: &sylvander_llm_anthropic::api::types::Usage,
) -> Option<u64> {
    fn component(tokens: u32, rate: u64) -> u128 {
        // rate is micro-USD / 1M tokens; nano-USD therefore divides by 1,000.
        (u128::from(tokens) * u128::from(rate) + 500) / 1_000
    }

    let cache_write = usage.cache_creation_input_tokens.unwrap_or(0);
    let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
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
    /// Start building an [`AgentRun`].
    #[must_use]
    pub fn builder(spec: AgentSpec, client: AnthropicClient) -> AgentRunBuilder {
        AgentRunBuilder::new(spec, client)
    }

    /// Build a run around a provider-neutral backend. Alternate qualified
    /// models remain fail-closed until the runtime catalog is provider-aware.
    #[must_use]
    pub fn provider_builder(
        spec: AgentSpec,
        provider: Arc<dyn ModelProvider>,
        model: ProviderModelInfo,
    ) -> AgentRunBuilder {
        AgentRunBuilder::new_single_provider(spec, provider, model)
    }

    /// Build a run around an immutable provider-qualified router.
    #[must_use]
    pub fn qualified_router_builder(
        spec: AgentSpec,
        router: Arc<dyn ModelProvider>,
        model: ProviderModelInfo,
    ) -> AgentRunBuilder {
        AgentRunBuilder::new_qualified_router(spec, router, model)
    }

    /// Unique agent identifier.
    #[must_use]
    pub fn id(&self) -> &AgentId {
        &self.inner.id
    }

    pub async fn runtime_model_info(&self) -> sylvander_protocol::RuntimeModelInfo {
        let runtime = self.inner.runtime_models.read().await;
        runtime.public_info()
    }

    /// Select the model configuration used by subsequently started turns.
    /// Existing turns continue with the immutable snapshot they started with.
    pub async fn select_model(
        &self,
        model_id: &str,
        reasoning_effort: sylvander_protocol::ReasoningEffort,
    ) -> Result<sylvander_protocol::RuntimeModelInfo, String> {
        let selection = self
            .inner
            .runtime_models
            .read()
            .await
            .resolve_legacy_id(model_id)?;
        self.select_qualified_model(selection, reasoning_effort)
            .await
    }

    /// Select one exact provider-qualified model for subsequently started turns.
    pub async fn select_qualified_model(
        &self,
        selection: sylvander_protocol::ModelSelection,
        reasoning_effort: sylvander_protocol::ReasoningEffort,
    ) -> Result<sylvander_protocol::RuntimeModelInfo, String> {
        let mut runtime = self.inner.runtime_models.write().await;
        let model = runtime.available.get(&selection).cloned().ok_or_else(|| {
            format!(
                "model `{}/{}` is not available",
                selection.provider_id, selection.model_id
            )
        })?;
        self.inner
            .prepare_loop_snapshot(&model, reasoning_effort)
            .map_err(|error| error.to_string())?;
        if reasoning_effort != sylvander_protocol::ReasoningEffort::Off
            && !model
                .shadow
                .capabilities
                .contains(ModelCapabilities::EXTENDED_THINKING)
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

    pub async fn permission_profile(&self) -> sylvander_protocol::PermissionProfile {
        self.inner.runtime_permissions.read().await.clone()
    }

    /// Return redacted, read-only platform truth for UI inspection. This does
    /// not probe or start optional services and never exposes MCP environment
    /// values or memory store paths.
    #[must_use]
    pub fn platform_snapshot(&self) -> sylvander_protocol::PlatformSnapshot {
        use sylvander_protocol::{
            PlatformAuthStatus, PlatformFeature, PlatformFeatureKind, PlatformFeatureStatus,
            PlatformTrust,
        };

        let mut features = self
            .inner
            .spec
            .mcp_servers
            .iter()
            .map(|server| PlatformFeature {
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
            .collect::<Vec<_>>();

        for runtime_feature in self.inner.loop_config.tools.platform_features() {
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
            .map(|command| sylvander_protocol::UiCommandDescriptor {
                id: command.id.clone(),
                name: command.name.clone(),
                usage: command.usage.clone(),
                description: command.description.clone(),
                hint: command.hint.clone(),
                source: "agent configuration".into(),
                trust: PlatformTrust::Workspace,
                effect: sylvander_protocol::UiCommandEffect::SubmitPrompt {
                    template: command.prompt.clone(),
                },
            })
            .collect();

        let tool_presentations = self
            .inner
            .spec
            .tool_presentations
            .iter()
            .map(
                |presentation| sylvander_protocol::ToolPresentationDescriptor {
                    tool_name: presentation.tool_name.clone(),
                    label: presentation.label.clone(),
                    kind: presentation.kind,
                    target_field: presentation.target_field.clone(),
                    source: "agent configuration".into(),
                    trust: PlatformTrust::Workspace,
                },
            )
            .collect();

        sylvander_protocol::PlatformSnapshot {
            features,
            commands,
            tool_presentations,
        }
    }

    pub async fn context_report(
        &self,
        session_id: Option<&SessionId>,
    ) -> sylvander_protocol::ContextReport {
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
            sources.push(sylvander_protocol::ContextSource {
                kind: sylvander_protocol::ContextSourceKind::SystemPrompt,
                label: "agent instructions".into(),
                items: 1,
            });
        }
        if conversation_items > 0 {
            sources.push(sylvander_protocol::ContextSource {
                kind: sylvander_protocol::ContextSourceKind::Conversation,
                label: "conversation messages".into(),
                items: conversation_items,
            });
        }
        let tool_count = self.inner.loop_config.tools.len();
        if tool_count > 0 {
            sources.push(sylvander_protocol::ContextSource {
                kind: sylvander_protocol::ContextSourceKind::Tools,
                label: "tool definitions".into(),
                items: tool_count,
            });
        }
        sylvander_protocol::ContextReport {
            model: model.shadow.id.clone(),
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
    ) -> Result<sylvander_protocol::CompactionReport, String> {
        self.compact_session_typed(session_id)
            .await
            .map_err(|error| error.compatibility_reason().into())
    }

    async fn compact_session_typed(
        &self,
        session_id: &SessionId,
    ) -> Result<sylvander_protocol::CompactionReport, crate::compress::error::CompactionError> {
        use crate::compress::error::{CompactionError, CompactionFailureCode};
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
        let usage = sylvander_llm_anthropic::api::types::Usage {
            input_tokens: model.shadow.context_window,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let summarizer = self.inner.loop_config.auto_compact_llm();
        let mut context = crate::compress::CompressContext {
            messages: &mut history,
            last_usage: &usage,
            model_info: &model.shadow,
            auto_compact_llm: Some(&summarizer),
        };
        let report = crate::compress::layers::auto_compact::AutoCompactLayer::new()
            .with_trigger_ratio(0.0)
            .apply(&mut context)
            .await;
        if let Some(error) =
            crate::compress::layer::first_failure_error(std::slice::from_ref(&report))
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

    pub async fn preview_workspace_rollback(
        &self,
        session_id: &SessionId,
    ) -> Result<crate::workspace_journal::RollbackPreview, String> {
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

    pub async fn rollback_workspace_latest(
        &self,
        session_id: &SessionId,
        expected_turn_id: &str,
    ) -> Result<crate::workspace_journal::RollbackReport, String> {
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
        profile: sylvander_protocol::PermissionProfile,
    ) -> Result<sylvander_protocol::PermissionProfile, String> {
        if profile.approval_policy == sylvander_protocol::ApprovalPolicy::Ask
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
        let ctx = self
            .inner
            .restore_session_context(&lease.session_id, &lease.metadata)
            .await;
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
                MessageKind::System(sys_msg) => match sys_msg {
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
                        let ctx = self
                            .inner
                            .restore_session_context(session_id, metadata)
                            .await;
                        self.inner
                            .sessions
                            .write()
                            .await
                            .insert(session_id.clone(), ctx);
                        info!(agent_id = %self.inner.id, %session_id, "joined session");
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
                                        Ok(()) => crate::approval::ApprovalDecision::Approved,
                                        Err(reason) => {
                                            crate::approval::ApprovalDecision::Rejected { reason }
                                        }
                                    }
                                } else {
                                    crate::approval::ApprovalDecision::Rejected {
                                        reason: format!(
                                            "approval scope `{scope:?}` is not permitted"
                                        ),
                                    }
                                }
                            } else {
                                crate::approval::ApprovalDecision::Rejected {
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
                            let _ = request.sender.send(decision.clone());
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
                },

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
    pub fn memory_tools(&self) -> Vec<Arc<dyn Tool>> {
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
        let caller = sylvander_protocol::SessionContext::new(
            session.metadata.user_id.clone(),
            self.inner.id.clone(),
            session_id.clone(),
        );
        Ok(MemoryExecutionContext::application_worker(&caller))
    }

    /// System-driven memory write (NOT a tool). Ownership is derived from a
    /// session already attached to this Agent application.
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

    /// Persist a structured application-derived memory for an attached
    /// session. Caller-controlled identity is deliberately absent.
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
        store.append_relationship(&context, append).await
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
                    sylvander_protocol::InteractionTimeoutKind::Approval,
                    &call_id,
                    120,
                    sylvander_protocol::TimeoutRecovery::RetryRequest,
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
    kind: sylvander_protocol::InteractionTimeoutKind,
    subject_id: &str,
    timeout_secs: u64,
    recovery: sylvander_protocol::TimeoutRecovery,
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

fn compaction_summary(layers: &[crate::compress::layer::LayerReport]) -> Option<String> {
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
    layers: &[crate::compress::layer::LayerReport],
) -> sylvander_protocol::CompactionReport {
    sylvander_protocol::CompactionReport {
        automatic,
        removed_messages: crate::compress::layer::total_removed(layers),
        condensed_blocks: crate::compress::layer::total_condensed(layers),
        freed_tokens: crate::compress::layer::total_freed(layers),
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
                sylvander_protocol::InteractionTimeoutKind::Question,
                call_id,
                300,
                sylvander_protocol::TimeoutRecovery::RetryRequest,
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
    async fn review(&self, plan_id: &str, steps: Vec<String>) -> crate::bus::PlanDecision {
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
                sylvander_protocol::InteractionTimeoutKind::Plan,
                plan_id,
                300,
                sylvander_protocol::TimeoutRecovery::RetryRequest,
            )
            .await;
            crate::bus::PlanDecision::Rejected {
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
    loop_config: AgentLoop,
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
        let loop_config = self.loop_config.clone();
        let tasks = self.tasks.clone();
        let running_id = task_id.clone();
        tokio::spawn(async move {
            use futures_util::StreamExt;
            let history = vec![sylvander_llm_anthropic::api::types::MessageParam::user(
                prompt,
            )];
            let mut stream = Box::pin(loop_::run_stream(&loop_config, history));
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
                            sylvander_protocol::InteractionTimeoutKind::Task,
                            &running_id,
                            600,
                            sylvander_protocol::TimeoutRecovery::NarrowScope,
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
                    crate::event::AgentEvent::IterationStart { iteration } => {
                        Some(StreamEvent::TaskProgress {
                            task_id: running_id.clone(),
                            message: format!("iteration {iteration}"),
                        })
                    }
                    crate::event::AgentEvent::ToolCallStart { name, .. } => {
                        Some(StreamEvent::TaskProgress {
                            task_id: running_id.clone(),
                            message: format!("running {name}"),
                        })
                    }
                    crate::event::AgentEvent::Done(message) => Some(StreamEvent::TaskCompleted {
                        task_id: running_id.clone(),
                        summary: message.text(),
                    }),
                    crate::event::AgentEvent::Error(error) => Some(StreamEvent::TaskFailed {
                        task_id: running_id.clone(),
                        error: error.to_string(),
                    }),
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
        let subject = UserProfileSubject::authenticated(
            sylvander_protocol::UserId::new(metadata.user_id.clone()),
            self.id.clone(),
            session_id.clone(),
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

    fn prepare_loop_snapshot(
        &self,
        model: &RuntimeModel,
        reasoning_effort: sylvander_protocol::ReasoningEffort,
    ) -> Result<AgentLoop, AgentRunError> {
        if reasoning_effort != sylvander_protocol::ReasoningEffort::Off
            && !model
                .shadow
                .capabilities
                .contains(ModelCapabilities::EXTENDED_THINKING)
        {
            return Err(AgentRunError::Configuration(format!(
                "model `{}` does not support reasoning effort",
                model.selection.model_id
            )));
        }
        let mut snapshot = self.loop_config.clone();
        snapshot
            .apply_runtime_model(&model.selection, &model.shadow, model.exact.as_ref())
            .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
        snapshot.reasoning_effort = reasoning_effort;
        Ok(snapshot)
    }

    async fn apply_compacted_history(
        &self,
        session_id: &SessionId,
        history: &[sylvander_llm_anthropic::api::types::MessageParam],
        layers: &[crate::compress::layer::LayerReport],
    ) -> Result<(), String> {
        let metadata = {
            let mut sessions = self.sessions.write().await;
            let Some(session) = sessions.get_mut(session_id) else {
                return Err(format!("unknown session: {session_id}"));
            };
            session.history = history.to_vec();
            session.updated_at = now_secs();
            session.metadata.clone()
        };
        self.context_usage.write().await.remove(session_id);
        let Some(summary) = compaction_summary(layers) else {
            return Ok(());
        };
        let Some(store) = &self.session_store else {
            return Ok(());
        };
        let caller = sylvander_protocol::SessionContext::new(
            metadata.user_id,
            self.id.clone(),
            session_id.clone(),
        );
        let result = async {
            let mut replacement = Vec::with_capacity(history.len());
            for (index, message) in history.iter().enumerate() {
                let content = serde_json::to_value(message).map_err(|error| {
                    crate::session_store::SessionStoreError::Store(error.to_string())
                })?;
                let role = match message.role {
                    sylvander_llm_anthropic::api::types::MessageRole::User => {
                        StoredMessageRole::User
                    }
                    sylvander_llm_anthropic::api::types::MessageRole::Assistant => {
                        StoredMessageRole::Assistant
                    }
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
        }
        .await;
        if let Err(error) = result {
            warn!(%session_id, %error, %summary, "failed to persist compacted history");
            return Err(format!(
                "compacted live context but failed to persist it: {error}"
            ));
        }
        Ok(())
    }

    async fn restore_session_context(
        &self,
        session_id: &SessionId,
        metadata: &SessionMetadata,
    ) -> SessionContext {
        let mut context = SessionContext::new(session_id.clone(), metadata.clone());
        let Some(store) = &self.session_store else {
            return context;
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
                stored.effective_config = Some(self.legacy_session_config(metadata).await);
                if let Err(error) = store.save(&stored).await {
                    warn!(%session_id, %error, "failed to persist joined session");
                    return context;
                }
            }
            Ok(Some(stored)) => {
                context.metadata = stored.metadata;
            }
            Err(error) => {
                warn!(%session_id, %error, "failed to inspect joined session");
                return context;
            }
        }

        let caller = sylvander_protocol::SessionContext::new(
            metadata.user_id.clone(),
            self.id.clone(),
            session_id.clone(),
        );
        match store.read_history(&caller, session_id, false, None).await {
            Ok(messages) => {
                for stored in messages {
                    match serde_json::from_value(stored.content) {
                        Ok(message) => context.history.push(message),
                        Err(error) => warn!(
                            %session_id,
                            seq = stored.seq,
                            %error,
                            "ignored malformed persisted message"
                        ),
                    }
                }
            }
            Err(error) => warn!(%session_id, %error, "failed to restore session history"),
        }
        context
    }

    async fn legacy_session_config(
        &self,
        metadata: &SessionMetadata,
    ) -> sylvander_protocol::SessionEffectiveConfig {
        let runtime = self.runtime_models.read().await;
        let source = || sylvander_protocol::SessionConfigSource {
            kind: sylvander_protocol::SessionConfigSourceKind::LegacyMigration,
            reference: Some("legacy-channel-session".into()),
        };
        let prompt = self
            .loop_config
            .system_prompt
            .as_deref()
            .unwrap_or_default();
        let resolved_prompt = self
            .prompt_resolver
            .as_ref()
            .and_then(|resolver| resolver.resolve(&runtime.current, None, None).ok());
        let (prompt_profile, system_prompt_sha256, prompt_manifest) = resolved_prompt.map_or_else(
            || {
                (
                    None,
                    format!("{:x}", Sha256::digest(prompt.as_bytes())),
                    None,
                )
            },
            |resolved| {
                (
                    resolved.profile_id,
                    resolved.system_prompt_sha256,
                    Some(resolved.manifest),
                )
            },
        );
        sylvander_protocol::SessionEffectiveConfig {
            agent_id: self.id.clone(),
            agent_revision: 0,
            provider_id: runtime.current.provider_id.clone(),
            provider_revision: None,
            model_id: runtime.current.model_id.clone(),
            model_revision: None,
            reasoning_effort: runtime.reasoning_effort,
            permissions: self.runtime_permissions.read().await.clone(),
            prompt_profile,
            system_prompt_sha256,
            prompt_manifest,
            agent_workspace: None,
            user_workspace: Some(sylvander_protocol::SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: metadata.workspace.clone(),
                read_only: false,
                instruction_focus: None,
            }),
            workspace_mounts: Vec::new(),
            execution_target: "local".into(),
            provenance: sylvander_protocol::SessionConfigProvenance {
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
                let _ = request.sender.send(crate::bus::PlanDecision::Rejected {
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
        let span = tracing::info_span!(
            "agent_turn",
            agent_id = %self.id,
            session_id = %msg.session_id,
            turn_id = %correlation.turn,
            request_id = %correlation.request,
            trace_id = %correlation.trace,
        );
        async {
            info!("turn started");
            let result = self
                .handle_message_with_interrupt(msg, interrupted, &correlation.turn)
                .await;
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
            store
                .get(&session_id)
                .await
                .map_err(|error| AgentRunError::Store(error.to_string()))?
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
                |config| sylvander_protocol::ModelSelection {
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
        let mut loop_config = self.prepare_loop_snapshot(&selected_model, selected_effort)?;
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
                        &selected_model.selection,
                        stored.config_overrides.prompt_profile.as_deref(),
                        stored.config_overrides.system_prompt.as_deref(),
                    )
                    .map_err(|_| prompt_integrity_error())?;
                if effective.system_prompt_sha256 != resolved_prompt.system_prompt_sha256
                    || effective.prompt_manifest.as_ref() != Some(&resolved_prompt.manifest)
                {
                    return Err(prompt_integrity_error());
                }
                prompt_policy
                    .turn_context_inputs(
                        &selected_model.selection,
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
                if let Some(prompt) = loop_config.system_prompt.take()
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
            &self.workspace_executors,
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
            let caller = sylvander_protocol::SessionContext::new(
                session_metadata.user_id.clone(),
                self.id.clone(),
                session_id.clone(),
            );
            let memory_context = MemoryExecutionContext::application_worker(&caller);
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

        let composed = compose_turn_context(context_inputs, &self.turn_context_budgets, now_secs())
            .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
        loop_config.system_prompt = Some(composed.system_prompt().to_owned());
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
            let caller = sylvander_protocol::SessionContext::new(
                user_id,
                self.id.clone(),
                session_id.clone(),
            );
            let user_content = serde_json::to_value(&user_message)
                .map_err(|error| AgentRunError::Store(error.to_string()))?;
            store
                .begin_turn(
                    &caller,
                    TurnStart {
                        session_id: session_id.clone(),
                        turn_id: turn_id.into(),
                        config_revision: stored.config_revision,
                        effective_config: effective.clone(),
                        user_content,
                        model_id: selected_model.shadow.id.clone(),
                    },
                )
                .await
                .map_err(|error| AgentRunError::Store(error.to_string()))?;
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
        let (turn_tools, capability_revision) = loop_config.tools.freeze_with_revision();
        loop_config.tools = turn_tools;
        let identity_authorized = self
            .authenticated_sessions
            .read()
            .await
            .contains(&session_id);
        if permissions.approval_policy == sylvander_protocol::ApprovalPolicy::Ask {
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
                Arc::new(crate::approval::RuleBasedApprovalGate::new(
                    self.approval_rules.clone(),
                    bus_gate,
                ))
            };
            loop_config.approval_gate = Some(gate);
        }
        if permissions.approval_policy == sylvander_protocol::ApprovalPolicy::Deny {
            loop_config.approval_gate = Some(Arc::new(DenyAllApprovalGate));
        }
        let tool_context = tool_context_for_permissions(
            ToolSessionExecution {
                metadata: &session_metadata,
                effective_config: effective_config.as_ref(),
                workspace_executors: &self.workspace_executors,
            },
            &self.id,
            &session_id,
            &permissions,
            self.memory.is_some() && identity_authorized,
            self.workspace_journal.clone(),
            Some(turn_id),
        );
        loop_config.tool_context = tool_context.clone();
        loop_config.ask_user_gate = Some(Arc::new(BusAskUserGate {
            bus: self.bus.clone(),
            agent_id: self.id.clone(),
            session_id: session_id.clone(),
            pending_answers: self.pending_answers.clone(),
        }));
        loop_config.plan_gate = Some(Arc::new(BusPlanGate {
            bus: self.bus.clone(),
            agent_id: self.id.clone(),
            session_id: session_id.clone(),
            pending_plans: self.pending_plans.clone(),
        }));
        let mut background_loop = loop_config.clone();
        background_loop.tool_context = tool_context;
        background_loop.tools = background_loop.tools.retain_named(&["read", "memory_read"]);
        background_loop.approval_gate = None;
        background_loop.ask_user_gate = None;
        background_loop.plan_gate = None;
        background_loop.task_gate = None;
        loop_config.task_gate = Some(Arc::new(BusTaskGate {
            bus: self.bus.clone(),
            agent_id: self.id.clone(),
            session_id: session_id.clone(),
            loop_config: background_loop,
            tasks: self.background_tasks.clone(),
        }));

        // 3. Run loop with streaming
        use futures_util::StreamExt;
        let mut stream = Box::pin(loop_::run_stream(&loop_config, history));
        tokio::pin!(interrupted);
        let mut final_message: Option<sylvander_llm_anthropic::api::types::Message> = None;

        loop {
            let event = tokio::select! {
                biased;
                _ = &mut interrupted => {
                    self.cancel_pending_decisions(&session_id).await;
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::TurnInterrupted {
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
                crate::event::AgentEvent::TextChunk(text) => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::TextDelta { delta: text },
                    )
                    .await;
                }
                crate::event::AgentEvent::ThinkingChunk(text) => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::ThinkingDelta { delta: text },
                    )
                    .await;
                }
                crate::event::AgentEvent::ModelRetry {
                    attempt,
                    max_attempts,
                    delay_ms,
                    reason,
                    cause,
                } => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::ModelRetry {
                            attempt,
                            max_attempts,
                            delay_ms,
                            reason,
                            cause,
                        },
                    )
                    .await;
                }
                crate::event::AgentEvent::ToolCallStart { id, name, input } => {
                    if matches!(
                        name.as_str(),
                        "present_plan" | "update_plan" | "start_background_task"
                    ) {
                        continue;
                    }
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::ToolCall {
                            call_id: id,
                            tool_name: name,
                            input,
                        },
                    )
                    .await;
                }
                crate::event::AgentEvent::ToolCallOutputDelta { id, name, delta } => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::ToolOutputDelta {
                            call_id: id,
                            tool_name: name,
                            delta,
                        },
                    )
                    .await;
                }
                crate::event::AgentEvent::ToolTimedOut {
                    id,
                    name: _,
                    timeout_secs,
                } => {
                    publish_interaction_timeout(
                        &self.bus,
                        &session_id,
                        &self.id,
                        sylvander_protocol::InteractionTimeoutKind::Tool,
                        &id,
                        timeout_secs,
                        sylvander_protocol::TimeoutRecovery::NarrowScope,
                    )
                    .await;
                }
                crate::event::AgentEvent::ToolCallEnd {
                    id,
                    name,
                    output,
                    is_error,
                } => {
                    if matches!(
                        name.as_str(),
                        "present_plan" | "update_plan" | "start_background_task"
                    ) {
                        continue;
                    }
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::ToolResult {
                            call_id: id,
                            tool_name: name,
                            output,
                            is_error,
                        },
                    )
                    .await;
                }
                crate::event::AgentEvent::ToolRejected { id, name, reason } => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::ToolResult {
                            call_id: id,
                            tool_name: name,
                            output: reason,
                            is_error: true,
                        },
                    )
                    .await;
                }
                crate::event::AgentEvent::IterationStart { iteration } => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::IterationStart { iteration },
                    )
                    .await;
                }
                crate::event::AgentEvent::IterationEnd {
                    iteration,
                    usage,
                    provider_usage,
                } => {
                    self.context_usage.write().await.insert(
                        session_id.clone(),
                        ContextUsage {
                            used: provider_usage.total_input_tokens(),
                            cache_read: provider_usage.cache_read_input_tokens.unwrap_or(0),
                            cache_write: provider_usage.cache_creation_input_tokens.unwrap_or(0),
                        },
                    );
                    let mut input_tokens = u64::from(usage.input_tokens);
                    let mut output_tokens = u64::from(usage.output_tokens);
                    let iteration_cost = selected_pricing
                        .and_then(|pricing| usage_cost_nano_usd(pricing, &provider_usage));
                    let mut cost_nano_usd = iteration_cost;
                    if let Some(store) = &self.session_store {
                        match store
                            .record_usage(
                                &session_id,
                                provider_usage.input_tokens,
                                provider_usage.output_tokens,
                                iteration_cost,
                            )
                            .await
                        {
                            Ok(total) => {
                                input_tokens = total.input_tokens;
                                output_tokens = total.output_tokens;
                                cost_nano_usd = total.cost_nano_usd;
                            }
                            Err(error) => {
                                warn!(%session_id, %error, "failed to persist session usage");
                            }
                        }
                    }
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::IterationEnd {
                            iteration,
                            input_tokens: u32::try_from(input_tokens).unwrap_or(u32::MAX),
                            output_tokens: u32::try_from(output_tokens).unwrap_or(u32::MAX),
                            cost_nano_usd,
                        },
                    )
                    .await;
                }
                crate::event::AgentEvent::CompressionStarted => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::CompactionStarted { automatic: true },
                    )
                    .await;
                }
                // `BusAskUserGate` publishes the request when it installs the
                // pending answer. Forwarding the loop event too would stack
                // two identical TUI modals for one question.
                crate::event::AgentEvent::Compressed { .. }
                | crate::event::AgentEvent::AskUser { .. }
                | crate::event::AgentEvent::PlanProposed { .. }
                | crate::event::AgentEvent::PlanResolved { .. } => {}
                crate::event::AgentEvent::HistoryCompacted { layers, history } => {
                    let persisted = self
                        .apply_compacted_history(&session_id, &history, &layers)
                        .await;
                    if let Some(error) = crate::compress::layer::first_failure_error(&layers) {
                        self.publish_stream(
                            &session_id,
                            crate::bus::StreamEvent::CompactionFailed {
                                automatic: true,
                                reason: error.compatibility_reason().into(),
                            },
                        )
                        .await;
                    } else if persisted.is_err() {
                        self.publish_stream(
                            &session_id,
                            crate::bus::StreamEvent::CompactionFailed {
                                automatic: true,
                                reason: crate::compress::error::CompactionError::new(
                                    crate::compress::error::CompactionFailureCode::Persistence,
                                )
                                .compatibility_reason()
                                .into(),
                            },
                        )
                        .await;
                    } else {
                        self.publish_stream(
                            &session_id,
                            crate::bus::StreamEvent::CompactionCompleted {
                                report: public_compaction_report(true, &layers),
                            },
                        )
                        .await;
                    }
                }
                crate::event::AgentEvent::UserAnswer { call_id, answer } => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::UserAnswer { call_id, answer },
                    )
                    .await;
                }
                crate::event::AgentEvent::Done(msg) => {
                    final_message = Some(msg);
                }
                crate::event::AgentEvent::Error(e) => {
                    self.publish_error(&session_id, &e).await;
                    return Err(AgentRunError::Loop(e));
                }
            }
        }

        // 4. Write final message to session + publish Done
        if let Some(msg) = final_message {
            let text = msg.text();
            if let Some(store) = &self.session_store {
                let user_id = self.sessions.read().await.get(&session_id).map_or_else(
                    || "unix-client".into(),
                    |context| context.metadata.user_id.clone(),
                );
                let caller = sylvander_protocol::SessionContext::new(
                    user_id,
                    self.id.clone(),
                    session_id.clone(),
                );
                let message = sylvander_llm_anthropic::api::types::MessageParam::assistant_blocks(
                    msg.content.clone(),
                );
                if let Ok(content) = serde_json::to_value(message)
                    && let Err(error) = store
                        .append_message(
                            &caller,
                            &session_id,
                            StoredMessageRole::Assistant,
                            content,
                            Some(&msg.model),
                            None,
                            None,
                        )
                        .await
                {
                    warn!(%session_id, %error, "failed to persist assistant message");
                }
            }
            let mut sessions = self.sessions.write().await;
            if let Some(ctx) = sessions.get_mut(&session_id) {
                ctx.append_assistant_message(msg);
            }
            drop(sessions);
            self.publish_stream(&session_id, crate::bus::StreamEvent::Done { text })
                .await;
        }

        Ok(())
    }

    // -- helpers --

    async fn publish_stream(&self, session_id: &SessionId, event: crate::bus::StreamEvent) {
        let msg = BusMessage::stream_event(session_id.clone(), self.id.clone(), event);
        let _ = self.bus.publish(msg).await;
    }

    async fn publish_error(&self, session_id: &SessionId, err: &AgentLoopError) {
        let _ = self
            .bus
            .publish(BusMessage {
                session_id: session_id.clone(),
                sender: Sender::Agent(self.id.clone()),
                recipient: Recipient::Broadcast,
                kind: MessageKind::Chat,
                payload: format!("Error: {err}"),
                attachments: Vec::new(),
                timestamp: now_secs(),
                id: crate::bus::MessageId::new(),
            })
            .await;
    }

    fn message_to_param(msg: &BusMessage) -> sylvander_llm_anthropic::api::types::MessageParam {
        use sylvander_llm_anthropic::api::types::{ImageBlock, UserContentBlock};
        if msg.attachments.is_empty() {
            return sylvander_llm_anthropic::api::types::MessageParam::user(&msg.payload);
        }
        let mut blocks = Vec::new();
        if !msg.payload.is_empty() {
            blocks.push(UserContentBlock::text(&msg.payload));
        }
        for attachment in &msg.attachments {
            match &attachment.content {
                crate::bus::AttachmentContent::Text { text } => {
                    blocks.push(UserContentBlock::text(format!(
                        "Attached {:?} `{}` ({}):\n{}",
                        attachment.kind, attachment.name, attachment.mime_type, text
                    )));
                }
                crate::bus::AttachmentContent::Base64 { data } => {
                    let image = match attachment.mime_type.as_str() {
                        "image/png" => Some(ImageBlock::png(data.clone())),
                        "image/jpeg" => Some(ImageBlock::jpeg(data.clone())),
                        _ => None,
                    };
                    if let Some(image) = image {
                        blocks.push(UserContentBlock::text(format!(
                            "Attached image `{}`:",
                            attachment.name
                        )));
                        blocks.push(UserContentBlock::Image(image));
                    }
                }
            }
        }
        sylvander_llm_anthropic::api::types::MessageParam::user_blocks(blocks)
    }
}

struct WorkspaceTurnContext {
    authoritative: Option<TurnContextCandidate>,
    retrieved: Vec<TurnContextCandidate>,
}

#[allow(clippy::too_many_arguments)]
async fn workspace_turn_context(
    agent_workspace: Option<&sylvander_protocol::SessionWorkspaceBinding>,
    task_workspace: Option<&sylvander_protocol::SessionWorkspaceBinding>,
    workspace_mounts: &[sylvander_protocol::SessionWorkspaceMount],
    fallback_task_workspace: &Path,
    workspace_executors: &HashMap<String, Arc<dyn WorkspaceExecutor>>,
    skill_features: &std::sync::RwLock<Vec<sylvander_protocol::PlatformFeature>>,
    query: &str,
    budget: crate::turn_context::TurnContextBudget,
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
        .map(|target| workspace_context_executor(workspace_executors, target))
        .transpose()?;
    let task_executor = task_target
        .as_ref()
        .map(|target| workspace_context_executor(workspace_executors, target))
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
        .map(|skill| sylvander_protocol::PlatformFeature {
            kind: sylvander_protocol::PlatformFeatureKind::Skill,
            name: skill.name.clone(),
            status: match skill.status {
                workspace_context::SkillStatus::Active => {
                    sylvander_protocol::PlatformFeatureStatus::Active
                }
                workspace_context::SkillStatus::Disabled => {
                    sylvander_protocol::PlatformFeatureStatus::Configured
                }
                workspace_context::SkillStatus::Degraded => {
                    sylvander_protocol::PlatformFeatureStatus::Degraded
                }
            },
            summary: format!("{} ({})", skill.summary, skill.role),
            source: Some(format!("{}:{}", skill.target_id, skill.relative_path)),
            trust: Some(if skill.role == "agent-home" {
                sylvander_protocol::PlatformTrust::BuiltIn
            } else {
                sylvander_protocol::PlatformTrust::Workspace
            }),
            auth: sylvander_protocol::PlatformAuthStatus::NotRequired,
            capabilities: skill.capabilities.clone(),
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
                    sylvander_protocol::WorkspaceMountRole::AgentHome => "agent-home",
                    sylvander_protocol::WorkspaceMountRole::Task => "task",
                    sylvander_protocol::WorkspaceMountRole::Dependency => "dependency",
                    sylvander_protocol::WorkspaceMountRole::Artifact => "artifact",
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

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn with_workspace_context(
    prompt: String,
    agent_workspace: Option<&sylvander_protocol::SessionWorkspaceBinding>,
    task_workspace: Option<&sylvander_protocol::SessionWorkspaceBinding>,
    workspace_mounts: &[sylvander_protocol::SessionWorkspaceMount],
    fallback_task_workspace: &Path,
    workspace_executors: &HashMap<String, Arc<dyn WorkspaceExecutor>>,
    skill_features: &std::sync::RwLock<Vec<sylvander_protocol::PlatformFeature>>,
) -> Result<String, AgentRunError> {
    let workspace = workspace_turn_context(
        agent_workspace,
        task_workspace,
        workspace_mounts,
        fallback_task_workspace,
        workspace_executors,
        skill_features,
        "",
        TurnContextBudgets::default().workspace_knowledge,
    )
    .await?;
    Ok(workspace.authoritative.map_or(prompt.clone(), |context| {
        format!("{prompt}\n\n{}", context.content())
    }))
}

fn workspace_target(binding: &sylvander_protocol::SessionWorkspaceBinding) -> WorkspaceTarget {
    WorkspaceTarget {
        id: binding.execution_target.clone(),
        workspace_path: binding.path.clone(),
        read_only: true,
    }
}

fn workspace_context_executor<'a>(
    workspace_executors: &'a HashMap<String, Arc<dyn WorkspaceExecutor>>,
    target: &WorkspaceTarget,
) -> Result<&'a Arc<dyn WorkspaceExecutor>, AgentRunError> {
    workspace_executors.get(&target.id).ok_or_else(|| {
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
    effective_config: Option<&'a sylvander_protocol::SessionEffectiveConfig>,
    workspace_executors: &'a HashMap<String, Arc<dyn WorkspaceExecutor>>,
}

fn tool_context_for_permissions(
    execution: ToolSessionExecution<'_>,
    agent_id: &AgentId,
    session_id: &SessionId,
    permissions: &sylvander_protocol::PermissionProfile,
    trusted_memory: bool,
    workspace_journal: Option<Arc<crate::workspace_journal::WorkspaceJournal>>,
    turn_id: Option<&str>,
) -> ToolContext {
    let metadata = execution.metadata;
    let mut session = sylvander_protocol::SessionContext::new(
        metadata.user_id.clone(),
        agent_id.clone(),
        session_id.clone(),
    );
    if let Some(turn_id) = turn_id {
        session = session.with_trace_id(turn_id);
    }
    let mut context = if trusted_memory {
        ToolContext::application(session)
    } else {
        ToolContext::new(session)
    };
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
    let permission_read_only =
        permissions.file_access != sylvander_protocol::FileAccess::WorkspaceWrite;
    let read_only = permission_read_only || binding.is_some_and(|binding| binding.read_only);
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
                    .workspace_executors
                    .get(target_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        Arc::new(UnavailableExecutor::new(target_id)) as Arc<dyn WorkspaceExecutor>
                    })
            },
            |config| {
                let default_reference = config
                    .workspace_mounts
                    .iter()
                    .find(|mount| mount.role == sylvander_protocol::WorkspaceMountRole::Task)
                    .or_else(|| config.workspace_mounts.first())
                    .map(|mount| mount.reference.clone())
                    .expect("non-empty workspace composition has a default mount");
                let mounts = config.workspace_mounts.iter().map(|mount| {
                    let mut capabilities = mount.capabilities;
                    if permission_read_only {
                        capabilities.write = false;
                        capabilities.command = false;
                    }
                    let executor = execution
                        .workspace_executors
                        .get(&mount.binding.execution_target)
                        .cloned()
                        .unwrap_or_else(|| {
                            Arc::new(UnavailableExecutor::new(
                                mount.binding.execution_target.clone(),
                            )) as Arc<dyn WorkspaceExecutor>
                        });
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
        context = context.with_workspace_journal(journal);
    }
    match permissions.file_access {
        sylvander_protocol::FileAccess::None => {}
        sylvander_protocol::FileAccess::ReadOnly => {
            context = context.with_capability(Cap::Read).with_capability(Cap::Git);
        }
        sylvander_protocol::FileAccess::WorkspaceWrite => {
            context = context
                .with_capability(Cap::Read)
                .with_capability(Cap::Write)
                .with_capability(Cap::Spawn)
                .with_capability(Cap::Git);
        }
    }
    if permissions.network_access == sylvander_protocol::NetworkAccess::Allowed {
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
    user_workspace: Option<&'a sylvander_protocol::SessionWorkspaceBinding>,
    agent_workspace: Option<&'a sylvander_protocol::SessionWorkspaceBinding>,
) -> Option<&'a sylvander_protocol::SessionWorkspaceBinding> {
    user_workspace.or(agent_workspace)
}

// ---------------------------------------------------------------------------
// AgentRunBuilder
// ---------------------------------------------------------------------------

/// Builder for [`AgentRun`].
pub struct AgentRunBuilder {
    spec: AgentSpec,
    backend: AgentRunModelBackend,
    bus: Option<Arc<dyn MessageBus>>,
    tool_overrides: Option<ToolRegistry>,
    compression_overrides: Option<crate::compress::pipeline::CompressionPipeline>,
    memory: Option<Arc<dyn MemoryStore>>,
    session_store: Option<Arc<dyn SessionStore>>,
    model_capabilities: Option<sylvander_llm_anthropic::api::model::ModelCapabilities>,
    available_models: Vec<ModelInfo>,
    available_provider_models: Vec<ProviderModelInfo>,
    legacy_model_lifecycles: HashMap<String, sylvander_protocol::ModelLifecycle>,
    legacy_model_pricing: HashMap<String, sylvander_protocol::ModelPricing>,
    qualified_model_lifecycles:
        HashMap<sylvander_protocol::ModelSelection, sylvander_protocol::ModelLifecycle>,
    qualified_model_pricing:
        HashMap<sylvander_protocol::ModelSelection, sylvander_protocol::ModelPricing>,
    prompt_resolver: Option<Arc<PromptResolver>>,
    user_profile_provider: Option<Arc<dyn UserProfileProvider>>,
    turn_context_budgets: TurnContextBudgets,
    approval_enabled: bool,
    approval_rules: Vec<crate::approval::ApprovalRule>,
    approval_store_path: Option<PathBuf>,
    workspace_journal_path: Option<PathBuf>,
    workspace_executors: HashMap<String, Arc<dyn WorkspaceExecutor>>,
}

enum AgentRunModelBackend {
    Legacy(AnthropicClient),
    SingleProvider {
        provider: Arc<dyn ModelProvider>,
        model: ProviderModelInfo,
    },
    QualifiedRouter {
        router: Arc<dyn ModelProvider>,
        model: ProviderModelInfo,
    },
}

impl AgentRunBuilder {
    fn new(spec: AgentSpec, client: AnthropicClient) -> Self {
        Self::with_backend(spec, AgentRunModelBackend::Legacy(client))
    }

    fn new_single_provider(
        spec: AgentSpec,
        provider: Arc<dyn ModelProvider>,
        model: ProviderModelInfo,
    ) -> Self {
        Self::with_backend(
            spec,
            AgentRunModelBackend::SingleProvider { provider, model },
        )
    }

    fn new_qualified_router(
        spec: AgentSpec,
        router: Arc<dyn ModelProvider>,
        model: ProviderModelInfo,
    ) -> Self {
        Self::with_backend(
            spec,
            AgentRunModelBackend::QualifiedRouter { router, model },
        )
    }

    fn with_backend(spec: AgentSpec, backend: AgentRunModelBackend) -> Self {
        let workspace_executors = HashMap::from([(
            "local".to_owned(),
            Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>,
        )]);
        Self {
            spec,
            backend,
            bus: None,
            tool_overrides: None,
            compression_overrides: None,
            memory: None,
            session_store: None,
            model_capabilities: None,
            available_models: Vec::new(),
            available_provider_models: Vec::new(),
            legacy_model_lifecycles: HashMap::new(),
            legacy_model_pricing: HashMap::new(),
            qualified_model_lifecycles: HashMap::new(),
            qualified_model_pricing: HashMap::new(),
            prompt_resolver: None,
            user_profile_provider: None,
            turn_context_budgets: TurnContextBudgets::default(),
            approval_enabled: false,
            approval_rules: Vec::new(),
            approval_store_path: None,
            workspace_journal_path: None,
            workspace_executors,
        }
    }

    #[must_use]
    pub fn bus(mut self, bus: Arc<dyn MessageBus>) -> Self {
        self.bus = Some(bus);
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

    #[must_use]
    pub fn override_tools(mut self, tools: ToolRegistry) -> Self {
        self.tool_overrides = Some(tools);
        self
    }

    #[must_use]
    pub fn model_capabilities(
        mut self,
        caps: sylvander_llm_anthropic::api::model::ModelCapabilities,
    ) -> Self {
        self.model_capabilities = Some(caps);
        self
    }

    /// Register alternate models reachable through the same configured
    /// provider client. The spec model remains the initial selection.
    #[must_use]
    pub fn available_models(mut self, models: Vec<ModelInfo>) -> Self {
        self.available_models = models;
        self
    }

    /// Register exact provider-qualified models. The configured provider
    /// adapter can route only entries belonging to its own provider.
    #[must_use]
    pub fn available_provider_models(mut self, models: Vec<ProviderModelInfo>) -> Self {
        self.available_provider_models = models;
        self
    }

    /// Attach operator-supplied lifecycle truth to advertised models.
    #[must_use]
    pub fn model_lifecycles(
        mut self,
        lifecycles: HashMap<String, sylvander_protocol::ModelLifecycle>,
    ) -> Self {
        self.legacy_model_lifecycles = lifecycles;
        self
    }

    /// Attach lifecycle truth to exact provider-qualified models.
    #[must_use]
    pub fn qualified_model_lifecycles(
        mut self,
        lifecycles: HashMap<sylvander_protocol::ModelSelection, sylvander_protocol::ModelLifecycle>,
    ) -> Self {
        self.qualified_model_lifecycles = lifecycles;
        self
    }

    /// Attach operator-supplied pricing snapshots to advertised models.
    #[must_use]
    pub fn model_pricing(
        mut self,
        pricing: HashMap<String, sylvander_protocol::ModelPricing>,
    ) -> Self {
        self.legacy_model_pricing = pricing;
        self
    }

    /// Attach pricing snapshots to exact provider-qualified models.
    #[must_use]
    pub fn qualified_model_pricing(
        mut self,
        pricing: HashMap<sylvander_protocol::ModelSelection, sylvander_protocol::ModelPricing>,
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
    pub fn approval_rules(mut self, rules: Vec<crate::approval::ApprovalRule>) -> Self {
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

    /// Register the adapter responsible for one exact execution target.
    ///
    /// `local` is registered by default and can be replaced explicitly (for
    /// example by a sandbox adapter). Unknown target ids remain unavailable.
    #[must_use]
    pub fn workspace_executor(
        mut self,
        target_id: impl Into<String>,
        executor: Arc<dyn WorkspaceExecutor>,
    ) -> Self {
        self.workspace_executors.insert(target_id.into(), executor);
        self
    }

    pub fn override_compression(
        mut self,
        pipeline: crate::compress::pipeline::CompressionPipeline,
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
        if self.workspace_executors.keys().any(String::is_empty) {
            return Err(AgentRunError::Build(
                "workspace executor target id must not be empty".into(),
            ));
        }
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

        let provider_backend = !matches!(&self.backend, AgentRunModelBackend::Legacy(_));
        let qualified_router =
            matches!(&self.backend, AgentRunModelBackend::QualifiedRouter { .. });
        let (mut model_info, primary_selection, primary_exact) = match &self.backend {
            AgentRunModelBackend::Legacy(_) => {
                let shadow = self.spec.to_model_info();
                let selection = sylvander_protocol::ModelSelection {
                    provider_id: self.spec.model.provider.clone(),
                    model_id: shadow.id.clone(),
                };
                (shadow, selection, None)
            }
            AgentRunModelBackend::SingleProvider { model, .. }
            | AgentRunModelBackend::QualifiedRouter { model, .. } => {
                if model.reference.provider != self.spec.model.provider
                    || model.reference.model != self.spec.model.model_name
                {
                    return Err(AgentRunError::Build(
                        "provider model does not match the Agent specification".into(),
                    ));
                }
                (
                    crate::provider_compat::model_metadata_from_core(model),
                    sylvander_protocol::ModelSelection {
                        provider_id: model.reference.provider.clone(),
                        model_id: model.reference.model.clone(),
                    },
                    Some(model.clone()),
                )
            }
        };
        if let Some(caps) = self.model_capabilities {
            if provider_backend {
                return Err(AgentRunError::Build(
                    "legacy capability overrides are unavailable for provider models".into(),
                ));
            }
            model_info.capabilities = caps;
        }
        if provider_backend && !self.available_models.is_empty() {
            return Err(AgentRunError::Build(
                "provider catalogs require exact provider model metadata".into(),
            ));
        }
        if qualified_router
            && (!self.legacy_model_lifecycles.is_empty() || !self.legacy_model_pricing.is_empty())
        {
            return Err(AgentRunError::Build(
                "qualified routers require provider-qualified model metadata".into(),
            ));
        }
        let mut catalog = Vec::new();
        if provider_backend {
            catalog.extend(self.available_provider_models.iter().map(|exact| {
                (
                    sylvander_protocol::ModelSelection {
                        provider_id: exact.reference.provider.clone(),
                        model_id: exact.reference.model.clone(),
                    },
                    crate::provider_compat::model_metadata_from_core(exact),
                    Some(exact.clone()),
                )
            }));
        } else {
            catalog.extend(self.available_models.iter().cloned().map(|shadow| {
                (
                    sylvander_protocol::ModelSelection {
                        provider_id: primary_selection.provider_id.clone(),
                        model_id: shadow.id.clone(),
                    },
                    shadow,
                    None,
                )
            }));
        }
        catalog.push((primary_selection.clone(), model_info.clone(), primary_exact));
        let available_models = catalog
            .into_iter()
            .map(|(selection, shadow, exact)| {
                let model = RuntimeModel {
                    lifecycle: self
                        .qualified_model_lifecycles
                        .get(&selection)
                        .or_else(|| self.legacy_model_lifecycles.get(&selection.model_id))
                        .cloned()
                        .unwrap_or_default(),
                    pricing: self
                        .qualified_model_pricing
                        .get(&selection)
                        .or_else(|| self.legacy_model_pricing.get(&selection.model_id))
                        .copied(),
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
            reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
        };
        let runtime_permissions = sylvander_protocol::PermissionProfile {
            file_access: sylvander_protocol::FileAccess::WorkspaceWrite,
            network_access: sylvander_protocol::NetworkAccess::Denied,
            approval_policy: if self.approval_enabled {
                sylvander_protocol::ApprovalPolicy::Ask
            } else {
                sylvander_protocol::ApprovalPolicy::Allow
            },
        };

        let mut loop_builder = match self.backend {
            AgentRunModelBackend::Legacy(client) => {
                AgentLoop::builder().client(client).model(model_info)
            }
            AgentRunModelBackend::SingleProvider { provider, model } => AgentLoop::builder()
                .provider(provider)
                .provider_model(model),
            AgentRunModelBackend::QualifiedRouter { router, model } => AgentLoop::builder()
                .qualified_router(router)
                .provider_model(model),
        }
        .max_iterations(self.spec.behavior.max_iterations)
        .max_retries(self.spec.behavior.max_retries);

        if !self.spec.persona.system_prompt.is_empty() {
            loop_builder = loop_builder.system_prompt(&self.spec.persona.system_prompt);
        }
        if let Some(tools) = self.tool_overrides {
            loop_builder = loop_builder.tools(tools);
        }
        if let Some(pipeline) = self.compression_overrides {
            loop_builder = loop_builder.compression_pipeline(pipeline);
        }

        let loop_config = loop_builder
            .build()
            .map_err(|e| AgentRunError::Build(format!("loop build failed: {e}")))?;

        let workspace_journal = self
            .workspace_journal_path
            .map(|path| Arc::new(crate::workspace_journal::WorkspaceJournal::new(path)));
        let session_authority = Arc::new(SessionAuthorityMarker);
        let issuer = AgentSessionIssuer {
            authority: session_authority.clone(),
        };
        let run = AgentRun {
            inner: Arc::new(AgentRunInner {
                id,
                spec: self.spec,
                loop_config,
                runtime_models: RwLock::new(runtime_models),
                runtime_permissions: RwLock::new(runtime_permissions),
                prompt_resolver: self.prompt_resolver,
                user_profile_provider: self.user_profile_provider,
                turn_context_budgets: self.turn_context_budgets,
                turn_context_manifests: RwLock::new(HashMap::new()),
                context_usage: RwLock::new(HashMap::new()),
                workspace_journal,
                workspace_executors: self.workspace_executors,
                skill_features: std::sync::RwLock::new(Vec::new()),
                bus,
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
    #[error("session store error: {0}")]
    Store(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../tests/unit/run.rs"]
mod tests;
