//! Agent runtime — the bridge between `AgentLoop` and the outside world.
//!
//! [`AgentRun`] is a running agent instance. It is a cheap `Clone` handle
//! to shared state ([`AgentRunInner`]).
//!
//! # Memory: mechanism first, tools second
//!
//! Memory is agent infrastructure. The *read* path is exposed as a tool
//! so the model can autonomously retrieve context. The *write* path is
//! system-driven via [`AgentRun::remember`].
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
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tracing::{Instrument as _, info, warn};

use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use sylvander_llm_core::{ModelInfo as ProviderModelInfo, ModelProvider};

use crate::approval::{ApprovalBatchResult, ApprovalDecision, ApprovalGate, ToolUseRequest};
use crate::ask_user_gate::AskUserGate;
use crate::bus::{
    AgentStatus as BusAgentStatus, BusMessage, MessageBus, MessageKind, Recipient, Sender,
    StreamEvent, SubscriptionFilter, SystemMessage, ToolCallInfo,
};
use crate::compress::layer::CompressionLayer;
use crate::error::AgentLoopError;
use crate::loop_::{self, AgentLoop};
use crate::plan_gate::PlanGate;
use crate::prompt::PromptResolver;
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
use crate::workspace_executor::{
    LocalExecutor, UnavailableExecutor, WorkspaceExecutor, WorkspaceTarget,
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
    /// Last provider-confirmed prompt usage for each session. This is window
    /// occupancy, unlike the durable cumulative billing counters.
    context_usage: RwLock<HashMap<SessionId, ContextUsage>>,
    workspace_journal: Option<Arc<crate::workspace_journal::WorkspaceJournal>>,
    /// Server-owned executor adapters keyed by exact execution-target id.
    workspace_executors: HashMap<String, Arc<dyn WorkspaceExecutor>>,
    /// Handle to the message bus.
    bus: Arc<dyn MessageBus>,
    /// Per-session conversation state.
    sessions: RwLock<HashMap<SessionId, SessionContext>>,
    /// Sessions whose identity was admitted through this run's private issuer.
    authenticated_sessions: RwLock<HashSet<SessionId>>,
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
    fingerprint: String,
    allowed_scopes: Vec<sylvander_protocol::ApprovalScope>,
    sender: oneshot::Sender<crate::approval::ApprovalDecision>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct PersistentApprovalFile {
    #[serde(default)]
    fingerprints: Vec<String>,
}

struct ApprovalMemory {
    sessions: HashMap<SessionId, HashSet<String>>,
    persistent: HashSet<String>,
    path: Option<PathBuf>,
}

impl ApprovalMemory {
    fn load(path: Option<PathBuf>) -> Result<Self, AgentRunError> {
        let persistent = match path.as_deref() {
            Some(path) if path.exists() => {
                let bytes = std::fs::read(path).map_err(|error| {
                    AgentRunError::Build(format!(
                        "failed to read approval store {}: {error}",
                        path.display()
                    ))
                })?;
                serde_json::from_slice::<PersistentApprovalFile>(&bytes)
                    .map_err(|error| {
                        AgentRunError::Build(format!(
                            "failed to parse approval store {}: {error}",
                            path.display()
                        ))
                    })?
                    .fingerprints
                    .into_iter()
                    .collect()
            }
            _ => HashSet::new(),
        };
        Ok(Self {
            sessions: HashMap::new(),
            persistent,
            path,
        })
    }

    fn allowed_scopes(&self) -> Vec<sylvander_protocol::ApprovalScope> {
        let mut scopes = vec![
            sylvander_protocol::ApprovalScope::Once,
            sylvander_protocol::ApprovalScope::Session,
        ];
        if self.path.is_some() {
            scopes.push(sylvander_protocol::ApprovalScope::Persistent);
        }
        scopes
    }

    fn contains(&self, session_id: &SessionId, fingerprint: &str) -> bool {
        self.persistent.contains(fingerprint)
            || self
                .sessions
                .get(session_id)
                .is_some_and(|entries| entries.contains(fingerprint))
    }

    async fn remember(
        &mut self,
        session_id: &SessionId,
        fingerprint: String,
        scope: sylvander_protocol::ApprovalScope,
    ) -> Result<(), String> {
        match scope {
            sylvander_protocol::ApprovalScope::Once => Ok(()),
            sylvander_protocol::ApprovalScope::Session => {
                self.sessions
                    .entry(session_id.clone())
                    .or_default()
                    .insert(fingerprint);
                Ok(())
            }
            sylvander_protocol::ApprovalScope::Persistent => {
                let path = self.path.clone().ok_or_else(|| {
                    "persistent approvals are disabled by the operator".to_string()
                })?;
                let inserted = self.persistent.insert(fingerprint.clone());
                if let Err(error) = persist_approval_fingerprints(&path, &self.persistent).await {
                    if inserted {
                        self.persistent.remove(&fingerprint);
                    }
                    return Err(error);
                }
                Ok(())
            }
        }
    }
}

async fn persist_approval_fingerprints(
    path: &Path,
    fingerprints: &HashSet<String>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("failed to create approval store directory: {error}"))?;
    }
    let mut entries = fingerprints.iter().cloned().collect::<Vec<_>>();
    entries.sort();
    let bytes = serde_json::to_vec_pretty(&PersistentApprovalFile {
        fingerprints: entries,
    })
    .map_err(|error| format!("failed to encode approval store: {error}"))?;
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    tokio::fs::write(&temporary, bytes)
        .await
        .map_err(|error| format!("failed to write approval store: {error}"))?;
    tokio::fs::rename(&temporary, path)
        .await
        .map_err(|error| format!("failed to replace approval store: {error}"))
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

        sylvander_protocol::PlatformSnapshot { features, commands }
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

    #[cfg(test)]
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

    #[cfg(test)]
    fn authenticated_session_for_test(&self, session_id: SessionId) -> AuthenticatedSession {
        AuthenticatedSession {
            authority: self.inner.session_authority.clone(),
            session_id,
        }
    }

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
        let ctx = self
            .inner
            .restore_session_context(&lease.session_id, &lease.metadata)
            .await;
        self.inner
            .sessions
            .write()
            .await
            .insert(lease.session_id.clone(), ctx);
        self.inner
            .authenticated_sessions
            .write()
            .await
            .insert(lease.session_id.clone());
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
            .approval_memory
            .lock()
            .await
            .sessions
            .remove(session_id);
    }

    /// List all sessions.
    pub async fn list_sessions(&self) -> Vec<SessionId> {
        self.inner.sessions.read().await.keys().cloned().collect()
    }

    /// Get a session context.
    pub async fn get_session(&self, session_id: &SessionId) -> Option<SessionContext> {
        self.inner.sessions.read().await.get(session_id).cloned()
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
    pub(crate) async fn run(self, mut inbox: mpsc::UnboundedReceiver<BusMessage>) {
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
                            .authenticated_sessions
                            .read()
                            .await
                            .contains(session_id)
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
                        self.inner.sessions.write().await.remove(session_id);
                        self.inner
                            .authenticated_sessions
                            .write()
                            .await
                            .remove(session_id);
                        self.inner.context_usage.write().await.remove(session_id);
                        self.inner
                            .approval_memory
                            .lock()
                            .await
                            .sessions
                            .remove(session_id);
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
                                        .remember(&request.session_id, request.fingerprint, *scope)
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
        let allowed_scopes = self.approval_memory.lock().await.allowed_scopes();
        let mut requested_tools = Vec::new();

        for (index, tool) in tools.iter().enumerate() {
            let fingerprint = approval_fingerprint(tool);
            if self
                .approval_memory
                .lock()
                .await
                .contains(&self.session_id, &fingerprint)
            {
                decisions[index] = Some(ApprovalDecision::Approved);
                continue;
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.pending_approvals.lock().await.insert(
                (self.session_id.clone(), tool.call_id.clone()),
                PendingApproval {
                    session_id: self.session_id.clone(),
                    fingerprint,
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

fn approval_fingerprint(tool: &ToolUseRequest) -> String {
    format!(
        "{}:{}",
        tool.tool_name,
        serde_json::to_string(&canonical_json(&tool.input)).unwrap_or_default()
    )
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

fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            serde_json::Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), canonical_json(&map[key])))
                    .collect(),
            )
        }
        scalar => scalar.clone(),
    }
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
            }),
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
            let turn_prompt = prompt_policy
                .resolve_turn_system_prompt(
                    &selected_model.selection,
                    stored.config_overrides.prompt_profile.as_deref(),
                    stored.config_overrides.system_prompt.as_deref(),
                    user_profile.as_ref(),
                )
                .map_err(|_| prompt_integrity_error())?;
            loop_config.system_prompt = Some(
                with_workspace_context(
                    turn_prompt,
                    effective.agent_workspace.as_ref(),
                    effective.user_workspace.as_ref(),
                    session_metadata.workspace.as_path(),
                    &self.workspace_executors,
                )
                .await?,
            );
        } else if loop_config.system_prompt.is_some() || user_profile.is_some() {
            let mut prompt = loop_config.system_prompt.take().unwrap_or_default();
            if let Some(profile) = &user_profile {
                if !prompt.is_empty() {
                    prompt.push_str("\n\n");
                }
                prompt.push_str(profile.content());
            }
            loop_config.system_prompt = Some(
                with_workspace_context(
                    prompt,
                    None,
                    None,
                    session_metadata.workspace.as_path(),
                    &self.workspace_executors,
                )
                .await?,
            );
        }

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
        let history = {
            let mut sessions = self.sessions.write().await;
            let ctx = sessions
                .get_mut(&session_id)
                .ok_or_else(|| AgentRunError::UnknownSession(session_id.clone()))?;
            ctx.append_user_message(user_message);
            ctx.history_snapshot()
        };

        // 2. Build per-session approval gate and tool surface from one
        // permission snapshot. Changes made mid-turn apply to the next turn.
        if permissions.approval_policy == sylvander_protocol::ApprovalPolicy::Ask {
            let bus_gate: Arc<dyn ApprovalGate> = Arc::new(BusApprovalGate {
                bus: self.bus.clone(),
                agent_id: self.id.clone(),
                session_id: session_id.clone(),
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
        let memory_authorized = self
            .authenticated_sessions
            .read()
            .await
            .contains(&session_id);
        let tool_context = tool_context_for_permissions(
            ToolSessionExecution {
                metadata: &session_metadata,
                effective_config: effective_config.as_ref(),
                workspace_executors: &self.workspace_executors,
            },
            &self.id,
            &session_id,
            &permissions,
            self.memory.is_some() && memory_authorized,
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
        background_loop.tool_definitions = crate::loop_::tool_definitions_for_model(
            &background_loop.tools,
            &background_loop.model,
        );
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

async fn with_workspace_context(
    prompt: String,
    agent_workspace: Option<&sylvander_protocol::SessionWorkspaceBinding>,
    task_workspace: Option<&sylvander_protocol::SessionWorkspaceBinding>,
    fallback_task_workspace: &Path,
    workspace_executors: &HashMap<String, Arc<dyn WorkspaceExecutor>>,
) -> Result<String, AgentRunError> {
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
    let context = workspace_context::discover(
        agent_target.zip(agent_executor).map(|(target, executor)| {
            workspace_context::WorkspaceContextSource {
                executor: executor.as_ref(),
                target,
            }
        }),
        task_target.zip(task_executor).map(|(target, executor)| {
            workspace_context::WorkspaceContextSource {
                executor: executor.as_ref(),
                target,
            }
        }),
    )
    .await
    .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
    Ok(context.map_or(prompt.clone(), |context| format!("{prompt}\n\n{context}")))
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
        .workspace_executors
        .get(target_id)
        .cloned()
        .unwrap_or_else(|| {
            Arc::new(UnavailableExecutor::new(target_id)) as Arc<dyn WorkspaceExecutor>
        });
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

        let approval_memory = ApprovalMemory::load(self.approval_store_path.clone())?;
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
                context_usage: RwLock::new(HashMap::new()),
                workspace_journal,
                workspace_executors: self.workspace_executors,
                bus,
                sessions: RwLock::new(HashMap::new()),
                authenticated_sessions: RwLock::new(HashSet::new()),
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
mod tests {
    use super::*;
    use crate::bus::InProcessMessageBus;
    use crate::tools::memory::InMemoryMemoryStore;
    use std::path::PathBuf;

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

    async fn next_stream_event(receiver: &mut mpsc::UnboundedReceiver<BusMessage>) -> StreamEvent {
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
        std::fs::create_dir_all(task.path().join(".agents/skills/test")).unwrap();
        std::fs::write(
            task.path().join(".agents/skills/test/SKILL.md"),
            "skill-guide",
        )
        .unwrap();

        let executors = HashMap::from([(
            "local".to_owned(),
            Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>,
        )]);
        let prompt = with_workspace_context(
            "base-prompt".into(),
            Some(&sylvander_protocol::SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: agent_home.path().to_path_buf(),
                read_only: true,
            }),
            Some(&sylvander_protocol::SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: task.path().to_path_buf(),
                read_only: false,
            }),
            task.path(),
            &executors,
        )
        .await
        .unwrap();
        let base = prompt.find("base-prompt").unwrap();
        let agent = prompt.find("agent-home-guide").unwrap();
        let task = prompt.find("task-guide").unwrap();
        let skill = prompt.find("skill-guide").unwrap();
        assert!(base < agent && agent < task && task < skill);
    }

    #[derive(Default)]
    struct RecordingProvider {
        requests: std::sync::Mutex<Vec<sylvander_llm_core::ModelRequest>>,
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
        ) -> Result<Vec<u8>, crate::workspace_executor::WorkspaceExecutorError> {
            self.reads.lock().unwrap().push(target.clone());
            Ok(self.marker.to_vec())
        }

        async fn write_file(
            &self,
            _target: &WorkspaceTarget,
            _relative_path: &str,
            _content: &[u8],
        ) -> Result<(), crate::workspace_executor::WorkspaceExecutorError> {
            Ok(())
        }

        async fn run_command(
            &self,
            _target: &WorkspaceTarget,
            _command: &str,
            _timeout: std::time::Duration,
        ) -> Result<
            crate::workspace_executor::WorkspaceCommandOutput,
            crate::workspace_executor::WorkspaceExecutorError,
        > {
            Ok(crate::workspace_executor::WorkspaceCommandOutput {
                success: true,
                status_code: Some(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                stdout_total_bytes: 0,
                stderr_total_bytes: 0,
            })
        }

        async fn list(
            &self,
            _target: &WorkspaceTarget,
            request: crate::workspace_executor::WorkspaceListRequest,
        ) -> Result<
            crate::workspace_executor::WorkspaceListResult,
            crate::workspace_executor::WorkspaceExecutorError,
        > {
            let entries = (request.relative_path == ".")
                .then(|| crate::workspace_executor::WorkspaceListEntry {
                    relative_path: "AGENTS.md".into(),
                    kind: crate::workspace_executor::WorkspaceEntryKind::File,
                    size: self.marker.len() as u64,
                })
                .into_iter()
                .collect();
            Ok(crate::workspace_executor::WorkspaceListResult {
                entries,
                truncated: false,
            })
        }
    }

    #[tokio::test]
    async fn workspace_prompt_uses_each_execution_target_without_local_filesystem_access() {
        let agent = Arc::new(MarkerWorkspaceExecutor::new(b"remote-agent-guide"));
        let task = Arc::new(MarkerWorkspaceExecutor::new(b"remote-task-guide"));
        let executors = HashMap::from([
            (
                "ssh:agent".to_owned(),
                agent.clone() as Arc<dyn WorkspaceExecutor>,
            ),
            (
                "ssh:task".to_owned(),
                task.clone() as Arc<dyn WorkspaceExecutor>,
            ),
        ]);
        let prompt = with_workspace_context(
            "base".into(),
            Some(&sylvander_protocol::SessionWorkspaceBinding {
                execution_target: "ssh:agent".into(),
                path: "/remote/agent".into(),
                read_only: true,
            }),
            Some(&sylvander_protocol::SessionWorkspaceBinding {
                execution_target: "ssh:task".into(),
                path: "/remote/task".into(),
                read_only: false,
            }),
            Path::new("/attached/task"),
            &executors,
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
    ) -> sylvander_protocol::SessionEffectiveConfig {
        let source = || sylvander_protocol::SessionConfigSource {
            kind: sylvander_protocol::SessionConfigSourceKind::LegacyMigration,
            reference: None,
        };
        sylvander_protocol::SessionEffectiveConfig {
            agent_id: AgentId::new("test-agent"),
            agent_revision: 0,
            provider_id: "test".into(),
            provider_revision: None,
            model_id: "test".into(),
            model_revision: None,
            reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
            permissions: sylvander_protocol::PermissionProfile::default(),
            prompt_profile: None,
            system_prompt_sha256: String::new(),
            prompt_manifest: None,
            agent_workspace: None,
            user_workspace: Some(sylvander_protocol::SessionWorkspaceBinding {
                execution_target: target_id.into(),
                path: workspace.into(),
                read_only: false,
            }),
            execution_target: target_id.into(),
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
                    sylvander_llm_core::ModelStreamEvent::Completed(response),
                )])) as sylvander_llm_core::ModelEventStream)
            })
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
            crate::session_store::SqliteSessionStore::open_in_memory()
                .await
                .unwrap(),
        );
        let (spec, _) = test_spec_and_client();
        let resolver = Arc::new(
            crate::prompt::PromptResolver::new(
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
        let run = AgentRun::provider_builder(spec, provider.clone(), model)
            .bus(Arc::new(InProcessMessageBus::new()))
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
        stored.effective_config = Some(run.inner.legacy_session_config(&metadata).await);
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

        let requests = provider.requests.lock().unwrap();
        let system = requests[0]
            .system
            .iter()
            .map(|instruction| instruction.text.as_str())
            .collect::<String>();
        assert!(system.contains("effective-worktree-guide"));
        assert!(!system.contains("source-workspace-guide"));
    }

    #[tokio::test]
    async fn identity_and_prompt_integrity_fail_before_provider_and_durable_turn_writes() {
        #[derive(Clone, Copy)]
        enum Tamper {
            SenderIdentity,
            SystemHash,
            LayerHash,
            MissingManifest,
        }

        for tamper in [
            Tamper::SenderIdentity,
            Tamper::SystemHash,
            Tamper::LayerHash,
            Tamper::MissingManifest,
        ] {
            let directory = tempfile::TempDir::new().expect("temporary directory");
            let database = directory.path().join("sessions.db");
            let store: Arc<dyn SessionStore> = Arc::new(
                crate::session_store::SqliteSessionStore::open(&database)
                    .await
                    .expect("store"),
            );
            let (spec, _) = test_spec_and_client();
            let selection = sylvander_protocol::ModelSelection {
                provider_id: spec.model.provider.clone(),
                model_id: spec.model.model_name.clone(),
            };
            let resolver = Arc::new(
                crate::prompt::PromptResolver::new(
                    "agent:test-agent@1".into(),
                    spec.persona.system_prompt.clone(),
                    Vec::new(),
                    None,
                    true,
                )
                .expect("prompt resolver"),
            );
            let prompt_snapshot = resolver
                .resolve(&selection, None, Some("private prompt sentinel"))
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
            let run = AgentRun::provider_builder(spec, provider.clone(), model)
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
            let mut effective = run.inner.legacy_session_config(&metadata).await;
            effective.agent_revision = 1;
            effective.system_prompt_sha256 = prompt_snapshot.system_prompt_sha256;
            effective.prompt_manifest = Some(prompt_snapshot.manifest);
            match tamper {
                Tamper::SenderIdentity => {}
                Tamper::SystemHash => effective.system_prompt_sha256 = "tampered".into(),
                Tamper::LayerHash => {
                    effective.prompt_manifest.as_mut().expect("manifest").layers[0].sha256 =
                        "tampered".into();
                }
                Tamper::MissingManifest => effective.prompt_manifest = None,
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
            for table in ["session_turn_configs", "session_messages"] {
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
        let run = AgentRun::provider_builder(spec, provider.clone(), provider_model)
            .bus(Arc::new(InProcessMessageBus::new()))
            .available_provider_models(vec![alternate, foreign])
            .build()
            .unwrap();

        let before = run.runtime_model_info().await;
        assert_eq!(before.models.len(), 3);
        assert!(
            run.select_model("shared", sylvander_protocol::ReasoningEffort::Off)
                .await
                .is_err()
        );
        assert!(
            run.select_qualified_model(
                sylvander_protocol::ModelSelection {
                    provider_id: "remote".into(),
                    model_id: "shared".into(),
                },
                sylvander_protocol::ReasoningEffort::Off,
            )
            .await
            .is_err()
        );
        assert_eq!(
            run.runtime_model_info().await.current_model,
            before.current_model
        );
        run.select_model("model-b", sylvander_protocol::ReasoningEffort::Off)
            .await
            .unwrap();
        let selected = {
            let runtime = run.inner.runtime_models.read().await;
            runtime.available.get(&runtime.current).unwrap().clone()
        };
        let snapshot = run
            .inner
            .prepare_loop_snapshot(&selected, sylvander_protocol::ReasoningEffort::Off)
            .unwrap();

        crate::loop_::run(
            &snapshot,
            vec![sylvander_llm_anthropic::api::types::MessageParam::user(
                "hello",
            )],
        )
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
        let local_selection = sylvander_protocol::ModelSelection {
            provider_id: "local".into(),
            model_id: "shared".into(),
        };
        let remote_selection = sylvander_protocol::ModelSelection {
            provider_id: "remote".into(),
            model_id: "shared".into(),
        };
        let remote_pricing = sylvander_protocol::ModelPricing {
            input_usd_micros_per_million: 11,
            output_usd_micros_per_million: 22,
            cache_write_usd_micros_per_million: None,
            cache_read_usd_micros_per_million: None,
        };
        let run = AgentRun::qualified_router_builder(spec, router.clone(), local)
            .bus(Arc::new(InProcessMessageBus::new()))
            .available_provider_models(vec![remote])
            .qualified_model_lifecycles(HashMap::from([
                (local_selection, sylvander_protocol::ModelLifecycle::Active),
                (
                    remote_selection.clone(),
                    sylvander_protocol::ModelLifecycle::Deprecated { replacement: None },
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
        assert_eq!(local.lifecycle, sylvander_protocol::ModelLifecycle::Active);
        assert_eq!(local.pricing, None);
        assert!(matches!(
            remote.lifecycle,
            sylvander_protocol::ModelLifecycle::Deprecated { .. }
        ));
        assert_eq!(remote.pricing, Some(remote_pricing));
        assert_eq!(
            remote.capability_names,
            [
                sylvander_protocol::ModelCapability::ToolUse,
                sylvander_protocol::ModelCapability::Vision,
            ]
        );

        run.select_qualified_model(remote_selection, sylvander_protocol::ReasoningEffort::Off)
            .await
            .unwrap();
        let selected = {
            let runtime = run.inner.runtime_models.read().await;
            runtime.available.get(&runtime.current).unwrap().clone()
        };
        let snapshot = run
            .inner
            .prepare_loop_snapshot(&selected, sylvander_protocol::ReasoningEffort::Off)
            .unwrap();
        crate::loop_::run(
            &snapshot,
            vec![sylvander_llm_anthropic::api::types::MessageParam::user(
                "hello",
            )],
        )
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
        let run = AgentRun::provider_builder(
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
                session.append_user_message(
                    sylvander_llm_anthropic::api::types::MessageParam::user(format!(
                        "message {index}"
                    )),
                );
            }
        }

        let report = run.compact_session(&session_id).await.unwrap();
        assert_eq!(report.removed_messages, 2);
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
        assert_eq!(run.get_session(&session_id).await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn manual_compaction_failures_are_typed_before_string_facade() {
        use crate::compress::error::CompactionFailureCode;

        let (spec, client) = test_spec_and_client();
        let run = AgentRun::builder(spec, client)
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
            .mcp_server_def(crate::spec::McpServerConfig {
                name: "search".into(),
                command: "/opt/bin/search-mcp".into(),
                args: vec!["--token".into(), "also-secret".into()],
                envs: std::collections::HashMap::from([(
                    "SEARCH_TOKEN".into(),
                    "super-secret".into(),
                )]),
            })
            .ui_command(crate::spec::UiCommandConfig {
                id: "security-review".into(),
                name: "security-review".into(),
                usage: "/security-review [scope]".into(),
                description: "Review a scope".into(),
                hint: "workspace".into(),
                prompt: "Review {{args}} for security issues.".into(),
            })
            .build()
            .unwrap();
        let client = AnthropicClient::builder()
            .api_key("test-key")
            .build()
            .unwrap();
        let run = AgentRun::builder(spec, client)
            .bus(Arc::new(InProcessMessageBus::new()))
            .memory(Arc::new(InMemoryMemoryStore::new()))
            .build()
            .unwrap();

        let snapshot = run.platform_snapshot();
        assert_eq!(snapshot.features.len(), 2);
        assert_eq!(snapshot.commands.len(), 1);
        assert_eq!(snapshot.commands[0].source, "agent configuration");
        assert_eq!(
            snapshot.commands[0].trust,
            sylvander_protocol::PlatformTrust::Workspace
        );
        assert_eq!(
            snapshot.features[0].status,
            sylvander_protocol::PlatformFeatureStatus::Configured
        );
        assert_eq!(
            snapshot.features[1].kind,
            sylvander_protocol::PlatformFeatureKind::Memory
        );
        assert_eq!(snapshot.features[1].name, "runtime memory");
        assert_eq!(
            snapshot.features[1].status,
            sylvander_protocol::PlatformFeatureStatus::Active
        );
        assert_eq!(
            snapshot.features[1].source.as_deref(),
            Some("runtime injection")
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
            .memory_store(crate::spec::MemoryStoreConfig {
                store_type: "sqlite".into(),
                path: PathBuf::from("/private/sentinel-memory.db"),
            })
            .build()
            .unwrap();
        let client = AnthropicClient::builder()
            .api_key("test-key")
            .build()
            .unwrap();
        let run = AgentRun::builder(spec, client)
            .bus(Arc::new(InProcessMessageBus::new()))
            .memory(Arc::new(InMemoryMemoryStore::new()))
            .build()
            .unwrap();

        let snapshot = run.platform_snapshot();
        let memory = snapshot
            .features
            .iter()
            .filter(|feature| feature.kind == sylvander_protocol::PlatformFeatureKind::Memory)
            .collect::<Vec<_>>();
        assert_eq!(memory.len(), 2);
        assert_eq!(
            memory
                .iter()
                .filter(|feature| {
                    feature.status == sylvander_protocol::PlatformFeatureStatus::Active
                })
                .count(),
            1
        );
        assert_eq!(memory[0].name, "runtime memory");
        assert_eq!(memory[1].name, "sqlite");
        assert_eq!(
            memory[1].status,
            sylvander_protocol::PlatformFeatureStatus::Configured
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
            .memory_store(crate::spec::MemoryStoreConfig {
                store_type: "unsupported-future-store".into(),
                path: PathBuf::from("/private/never-open-this-store"),
            })
            .build()
            .unwrap();
        let client = AnthropicClient::builder()
            .api_key("test-key")
            .build()
            .unwrap();
        let run = AgentRun::builder(spec, client)
            .bus(Arc::new(InProcessMessageBus::new()))
            .build()
            .unwrap();

        assert!(run.inner.memory.is_none());
        let snapshot = run.platform_snapshot();
        let memory = snapshot
            .features
            .iter()
            .filter(|feature| feature.kind == sylvander_protocol::PlatformFeatureKind::Memory)
            .collect::<Vec<_>>();
        assert_eq!(memory.len(), 1);
        assert_eq!(
            memory[0].status,
            sylvander_protocol::PlatformFeatureStatus::Configured
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
            pending_approvals: pending.clone(),
            approval_memory: Arc::new(Mutex::new(ApprovalMemory::load(None).unwrap())),
        });
        let request = ToolUseRequest {
            call_id: "tool-1".into(),
            tool_name: "write".into(),
            input: serde_json::json!({"path": "notes.md"}),
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
                kind: sylvander_protocol::InteractionTimeoutKind::Approval,
                subject_id,
                timeout_secs: 120,
                recovery: sylvander_protocol::TimeoutRecovery::RetryRequest,
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
                kind: sylvander_protocol::InteractionTimeoutKind::Question,
                subject_id,
                timeout_secs: 300,
                recovery: sylvander_protocol::TimeoutRecovery::RetryRequest,
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
            crate::bus::PlanDecision::Rejected { reason } if reason == "plan review timed out"
        ));
        assert!(pending.lock().await.is_empty());
        assert!(matches!(
            next_stream_event(&mut events).await,
            StreamEvent::InteractionTimedOut {
                kind: sylvander_protocol::InteractionTimeoutKind::Plan,
                subject_id,
                timeout_secs: 300,
                recovery: sylvander_protocol::TimeoutRecovery::RetryRequest,
            } if subject_id == "plan-1"
        ));
    }

    #[test]
    fn configured_pricing_calculates_nano_usd_and_requires_cache_rates() {
        let pricing = sylvander_protocol::ModelPricing {
            input_usd_micros_per_million: 3_000_000,
            output_usd_micros_per_million: 15_000_000,
            cache_write_usd_micros_per_million: None,
            cache_read_usd_micros_per_million: Some(300_000),
        };
        let mut usage = sylvander_llm_anthropic::api::types::Usage {
            input_tokens: 1_000,
            output_tokens: 100,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(10_000),
        };
        assert_eq!(usage_cost_nano_usd(pricing, &usage), Some(7_500_000));
        usage.cache_creation_input_tokens = Some(1);
        assert_eq!(usage_cost_nano_usd(pricing, &usage), None);
    }

    #[tokio::test]
    async fn agent_run_is_cloneable() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .build()
            .expect("build");
        let run2 = run.clone();
        assert_eq!(run.id(), run2.id());
    }

    #[tokio::test]
    async fn agent_run_previews_and_rolls_back_journaled_write() {
        use crate::tool::Tool;
        let workspace = tempfile::TempDir::new().unwrap();
        let journal = tempfile::TempDir::new().unwrap();
        let file = workspace.path().join("file.txt");
        std::fs::write(&file, "before").unwrap();
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let run = AgentRun::builder(spec, client)
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
            sylvander_protocol::SessionContext::new("user-1", "test-agent", session_id.clone())
                .with_trace_id("turn-1"),
        )
        .with_fs_root(workspace.path())
        .with_capability(Cap::Write)
        .with_workspace_journal(run.inner.workspace_journal.clone().unwrap());
        crate::tools::WriteTool::new(workspace.path())
            .execute(
                &context,
                serde_json::json!({"file_path":"file.txt","content":"after"}),
            )
            .await
            .unwrap();

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
        let thinking = ModelInfo::builder()
            .id("thinking-model")
            .context_window(200_000)
            .max_output_tokens(32_000)
            .capability(ModelCapabilities::EXTENDED_THINKING)
            .build()
            .expect("model");
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .available_models(vec![thinking])
            .model_lifecycles(HashMap::from([(
                "thinking-model".into(),
                sylvander_protocol::ModelLifecycle::Deprecated {
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
            Some(sylvander_protocol::ModelLifecycle::Deprecated {
                replacement: Some(replacement)
            }) if replacement == "claude-sonnet-5-20260601"
        ));
        let selected = run
            .select_model("thinking-model", sylvander_protocol::ReasoningEffort::High)
            .await
            .expect("select");
        assert_eq!(selected.current_model, "thinking-model");
        assert_eq!(
            selected.reasoning_effort,
            sylvander_protocol::ReasoningEffort::High
        );
        assert!(
            run.select_model(
                "claude-sonnet-5-20260601",
                sylvander_protocol::ReasoningEffort::Low,
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
        let run = AgentRun::builder(spec, client)
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
            .append_user_message(sylvander_llm_anthropic::api::types::MessageParam::user(
                "hello",
            ));
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
            source.kind == sylvander_protocol::ContextSourceKind::Conversation && source.items == 1
        }));
    }

    #[tokio::test]
    async fn runtime_permissions_are_validated_against_operator_capabilities() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .build()
            .expect("build");
        assert_eq!(
            run.permission_profile().await,
            sylvander_protocol::PermissionProfile::default()
        );
        let restricted = sylvander_protocol::PermissionProfile {
            file_access: sylvander_protocol::FileAccess::ReadOnly,
            network_access: sylvander_protocol::NetworkAccess::Denied,
            approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
        };
        assert_eq!(
            run.select_permissions(restricted.clone()).await.unwrap(),
            restricted
        );
        assert!(
            run.select_permissions(sylvander_protocol::PermissionProfile {
                approval_policy: sylvander_protocol::ApprovalPolicy::Ask,
                ..Default::default()
            })
            .await
            .is_err()
        );
    }

    #[test]
    fn permission_profile_builds_a_workspace_scoped_tool_context() {
        let metadata = test_metadata();
        let context = tool_context_for_permissions(
            ToolSessionExecution {
                metadata: &metadata,
                effective_config: None,
                workspace_executors: &HashMap::from([(
                    "local".to_owned(),
                    Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>,
                )]),
            },
            &AgentId::new("agent"),
            &SessionId::new("session"),
            &sylvander_protocol::PermissionProfile {
                file_access: sylvander_protocol::FileAccess::ReadOnly,
                network_access: sylvander_protocol::NetworkAccess::Allowed,
                approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
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
        assert_eq!(context.user_id().0, metadata.user_id);
        assert_eq!(context.session.request.trace_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn builder_registers_local_and_injected_workspace_executors() {
        let (spec, client) = test_spec_and_client();
        let remote: Arc<dyn WorkspaceExecutor> = Arc::new(MarkerWorkspaceExecutor::new(b"remote"));
        let run = AgentRun::builder(spec, client)
            .bus(Arc::new(InProcessMessageBus::new()))
            .workspace_executor("ssh:build", remote.clone())
            .build()
            .expect("build");

        assert!(run.inner.workspace_executors.contains_key("local"));
        assert!(Arc::ptr_eq(
            run.inner.workspace_executors.get("ssh:build").unwrap(),
            &remote
        ));
    }

    #[tokio::test]
    async fn turn_context_resolves_the_effective_execution_target() {
        let metadata = test_metadata();
        let effective = remote_effective_config("ssh:build", "/remote/project");
        let remote = Arc::new(MarkerWorkspaceExecutor::new(b"remote"));
        let executors = HashMap::from([(
            "ssh:build".to_owned(),
            remote.clone() as Arc<dyn WorkspaceExecutor>,
        )]);
        let context = tool_context_for_permissions(
            ToolSessionExecution {
                metadata: &metadata,
                effective_config: Some(&effective),
                workspace_executors: &executors,
            },
            &AgentId::new("agent"),
            &SessionId::new("session"),
            &sylvander_protocol::PermissionProfile::default(),
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
        let before_restart = AgentRun::builder(spec, client)
            .bus(Arc::new(InProcessMessageBus::new()))
            .workspace_executor("container:dev", old)
            .build()
            .unwrap();
        drop(before_restart);
        let (spec, client) = test_spec_and_client();
        let after_restart = AgentRun::builder(spec, client)
            .bus(Arc::new(InProcessMessageBus::new()))
            .workspace_executor("container:dev", new)
            .build()
            .unwrap();
        let permissions = sylvander_protocol::PermissionProfile::default();
        let context_after_restart = tool_context_for_permissions(
            ToolSessionExecution {
                metadata: &metadata,
                effective_config: Some(&effective),
                workspace_executors: &after_restart.inner.workspace_executors,
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
    async fn unknown_execution_target_is_explicitly_unavailable() {
        let metadata = test_metadata();
        let effective = remote_effective_config("ssh:missing", "/remote/project");
        let context = tool_context_for_permissions(
            ToolSessionExecution {
                metadata: &metadata,
                effective_config: Some(&effective),
                workspace_executors: &HashMap::new(),
            },
            &AgentId::new("agent"),
            &SessionId::new("session"),
            &sylvander_protocol::PermissionProfile::default(),
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
            crate::workspace_executor::WorkspaceExecutorError::Unavailable(target)
                if target == "ssh:missing"
        ));
    }

    #[test]
    fn user_workspace_precedes_agent_workspace_and_agent_fallback_keeps_read_only() {
        let user = sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: "/user".into(),
            read_only: false,
        };
        let agent = sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "ssh:agent".into(),
            path: "/agent".into(),
            read_only: true,
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
        let run = AgentRun::builder(spec, client)
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
        let run = AgentRun::builder(spec, client)
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
            run.inner.pending_approvals.lock().await.insert(
                (session.clone(), "shared-id".into()),
                PendingApproval {
                    session_id: session.clone(),
                    fingerprint: "shared-fingerprint".into(),
                    allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
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
                scope: sylvander_protocol::ApprovalScope::Once,
                reason: Some("session A rejected".into()),
            },
            SystemMessage::AnswerQuestion {
                call_id: "shared-id".into(),
                answer: "session A answer".into(),
            },
            SystemMessage::ResolvePlan {
                plan_id: "shared-id".into(),
                decision: sylvander_protocol::PlanDecision::Approved,
            },
        ] {
            bus.publish(BusMessage {
                session_id: session_a.clone(),
                sender: crate::bus::Sender::System,
                recipient: crate::bus::Recipient::Agent(AgentId::new("test-agent")),
                kind: MessageKind::System(kind),
                payload: String::new(),
                attachments: Vec::new(),
                timestamp: crate::session::now_secs(),
                id: crate::bus::MessageId::new(),
            })
            .await
            .unwrap();
        }

        assert!(matches!(
            approval_a_rx.await.unwrap(),
            ApprovalDecision::Rejected { reason } if reason == "session A rejected"
        ));
        assert_eq!(answer_a_rx.await.unwrap(), ["session A answer"]);
        assert_eq!(plan_a_rx.await.unwrap(), crate::bus::PlanDecision::Approved);
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

    #[tokio::test]
    async fn durable_session_history_restores_into_agent_context() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let agent_id = spec.id.clone();
        let store: Arc<dyn SessionStore> = Arc::new(
            crate::session_store::SqliteSessionStore::open_in_memory()
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
        let caller = sylvander_protocol::SessionContext::new(
            metadata.user_id.clone(),
            agent_id,
            session_id.clone(),
        );
        store
            .append_message(
                &caller,
                &session_id,
                StoredMessageRole::User,
                serde_json::to_value(sylvander_llm_anthropic::api::types::MessageParam::user(
                    "remember me",
                ))
                .expect("serialize"),
                None,
                None,
                None,
            )
            .await
            .expect("append");

        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .session_store(store)
            .build()
            .expect("build");
        let restored = run
            .inner
            .restore_session_context(&session_id, &metadata)
            .await;

        assert_eq!(restored.len(), 1);
    }

    #[tokio::test]
    async fn legacy_join_persists_an_auditable_effective_configuration() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let resolver = Arc::new(
            crate::prompt::PromptResolver::new(
                "agent:test-agent@1".into(),
                spec.persona.system_prompt.clone(),
                Vec::new(),
                None,
                false,
            )
            .expect("resolver"),
        );
        let store: Arc<dyn SessionStore> = Arc::new(
            crate::session_store::SqliteSessionStore::open_in_memory()
                .await
                .expect("store"),
        );
        let session_id = SessionId::new("legacy-session");
        let metadata = test_metadata();
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .session_store(store.clone())
            .prompt_resolver(resolver)
            .build()
            .expect("build");

        run.inner
            .restore_session_context(&session_id, &metadata)
            .await;

        let stored = store.get(&session_id).await.unwrap().unwrap();
        let effective = stored
            .effective_config
            .expect("legacy session must snapshot runtime defaults");
        assert_eq!(effective.agent_id, run.id().clone());
        assert!(effective.prompt_manifest.is_some());
        assert_eq!(effective.user_workspace.unwrap().path, metadata.workspace);
        assert_eq!(
            effective.provenance.model.kind,
            sylvander_protocol::SessionConfigSourceKind::LegacyMigration
        );
    }

    #[tokio::test]
    async fn compacted_history_replaces_runtime_and_durable_active_history() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let agent_id = spec.id.clone();
        let store: Arc<dyn SessionStore> = Arc::new(
            crate::session_store::SqliteSessionStore::open_in_memory()
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
        let caller = sylvander_protocol::SessionContext::new(
            metadata.user_id.clone(),
            agent_id,
            session_id.clone(),
        );
        for index in 0..6 {
            store
                .append_message(
                    &caller,
                    &session_id,
                    StoredMessageRole::User,
                    serde_json::to_value(sylvander_llm_anthropic::api::types::MessageParam::user(
                        format!("message {index}"),
                    ))
                    .expect("serialize"),
                    None,
                    None,
                    None,
                )
                .await
                .expect("append");
        }
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .session_store(store.clone())
            .build()
            .expect("build");
        run.inner.sessions.write().await.insert(
            session_id.clone(),
            SessionContext::new(session_id.clone(), metadata),
        );
        let history = vec![
            sylvander_llm_anthropic::api::types::MessageParam::user(
                "[Earlier conversation summary]\nimportant decisions",
            ),
            sylvander_llm_anthropic::api::types::MessageParam::user("recent one"),
            sylvander_llm_anthropic::api::types::MessageParam::user("recent two"),
        ];
        let layers = vec![crate::compress::layer::LayerReport {
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
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .memory(store)
            .build()
            .expect("build");
        let tools = run.memory_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "read_memory");
    }

    #[tokio::test]
    async fn session_capability_is_bound_to_one_run() {
        let (spec_a, client_a) = test_spec_and_client();
        let (run_a, issuer_a) = AgentRun::builder(spec_a, client_a)
            .bus(Arc::new(InProcessMessageBus::new()))
            .build_with_session_issuer()
            .expect("build A");
        let (spec_b, client_b) = test_spec_and_client();
        let (run_b, _) = AgentRun::builder(spec_b, client_b)
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
        let (_, issuer) = AgentRun::builder(spec, client)
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
        let run = AgentRun::builder(spec, client)
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
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .memory(store)
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
                crate::tools::memory::MemoryFilter::default(),
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
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .memory(store)
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
            crate::tools::memory::MemoryOwner::Relationship {
                user_id: sylvander_protocol::types::UserId::new("actual-user"),
                agent_id: run.id().clone(),
            }
        );
        assert_eq!(
            run.recall(
                &session,
                "caller-owned",
                crate::tools::memory::MemoryFilter::default(),
            )
            .await
            .unwrap()
            .len(),
            1
        );
    }

    #[tokio::test]
    async fn remember_fails_without_memory_configured() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let run = AgentRun::builder(spec, client)
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
        let run = AgentRun::builder(spec, client)
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
            vec![crate::bus::MessageAttachment {
                id: "a1".into(),
                kind: crate::bus::AttachmentKind::File,
                name: "src/main.rs".into(),
                mime_type: "text/x-rust".into(),
                content: crate::bus::AttachmentContent::Text {
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
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .build()
            .expect("build");
        let sid = run.join_session(test_metadata()).await;
        assert_eq!(run.list_sessions().await.len(), 1);
        run.leave_session(&sid).await;
        assert!(run.list_sessions().await.is_empty());
    }

    #[tokio::test]
    async fn approval_memory_is_session_isolated_and_persistent_only_with_a_store() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("approvals.json");
        let first = SessionId::new("session-1");
        let second = SessionId::new("session-2");
        let fingerprint = "write:{\"file_path\":\"a.rs\"}".to_string();

        let mut memory = ApprovalMemory::load(Some(path.clone())).expect("load");
        assert_eq!(
            memory.allowed_scopes(),
            vec![
                sylvander_protocol::ApprovalScope::Once,
                sylvander_protocol::ApprovalScope::Session,
                sylvander_protocol::ApprovalScope::Persistent,
            ]
        );
        memory
            .remember(
                &first,
                fingerprint.clone(),
                sylvander_protocol::ApprovalScope::Session,
            )
            .await
            .expect("session grant");
        assert!(memory.contains(&first, &fingerprint));
        assert!(!memory.contains(&second, &fingerprint));

        let durable = "read:{\"file_path\":\"README.md\"}".to_string();
        memory
            .remember(
                &first,
                durable.clone(),
                sylvander_protocol::ApprovalScope::Persistent,
            )
            .await
            .expect("persistent grant");
        let reloaded = ApprovalMemory::load(Some(path)).expect("reload");
        assert!(reloaded.contains(&second, &durable));
        assert!(!reloaded.contains(&first, &fingerprint));
    }

    #[test]
    fn approval_fingerprint_is_stable_across_json_key_order() {
        let first = ToolUseRequest {
            call_id: "a".into(),
            tool_name: "write".into(),
            input: serde_json::json!({"content": "x", "file_path": "a.rs"}),
        };
        let second = ToolUseRequest {
            call_id: "b".into(),
            tool_name: "write".into(),
            input: serde_json::json!({"file_path": "a.rs", "content": "x"}),
        };
        assert_eq!(approval_fingerprint(&first), approval_fingerprint(&second));
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
        let run = AgentRun::builder(spec, client)
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
}
