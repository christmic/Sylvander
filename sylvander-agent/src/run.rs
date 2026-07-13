//! Agent runtime — the bridge between AgentLoop and the outside world.
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

use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tracing::{info, warn};

use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};

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
use crate::session::{SessionContext, SessionMetadata, now_secs};
use crate::session_store::{
    MessageRole as StoredMessageRole, ReplacementMessage, SessionLifetime, SessionStore,
    StoredSession,
};
use crate::spec::{AgentId, AgentSpec, SessionId};
use crate::task_gate::TaskGate;
use crate::tool::{Tool, ToolRegistry};
use crate::tool_context::{Cap, NetworkPolicy, ToolContext};
use crate::tools::MemoryReadTool;
use crate::tools::memory::{MemoryEntry, MemoryStore, MemoryStoreError};

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
    /// Last provider-confirmed prompt usage for each session. This is window
    /// occupancy, unlike the durable cumulative billing counters.
    context_usage: RwLock<HashMap<SessionId, ContextUsage>>,
    /// Handle to the message bus.
    bus: Arc<dyn MessageBus>,
    /// Per-session conversation state.
    sessions: RwLock<HashMap<SessionId, SessionContext>>,
    /// Optional durable source of truth shared with channels/runtime.
    session_store: Option<Arc<dyn SessionStore>>,
    /// Long-term memory store.
    memory: Option<Arc<dyn MemoryStore>>,
    /// Whether bus-based approval is enabled (opt-in, off by default).
    approval_enabled: bool,
    /// Static approval rules (auto-approve/auto-reject).
    approval_rules: Vec<crate::approval::ApprovalRule>,
    /// Pending approval requests (shared with BusApprovalGate).
    pending_approvals: Arc<Mutex<HashMap<String, PendingApproval>>>,
    /// Agent-owned approval memory. Session grants are isolated by session;
    /// persistent grants exist only when the operator configured a store.
    approval_memory: Arc<Mutex<ApprovalMemory>>,
    /// Pending AskUser answers (shared with BusAskUserGate).
    pending_answers: Arc<Mutex<HashMap<String, PendingAnswer>>>,
    /// Pending typed plan decisions (shared with BusPlanGate).
    pending_plans: Arc<Mutex<HashMap<String, PendingPlan>>>,
    /// Independently cancellable read-only background runs.
    background_tasks: Arc<Mutex<HashMap<String, ActiveBackgroundTask>>>,
    /// Per-session concurrency locks (M12).
    session_locks: Mutex<HashMap<SessionId, Arc<Mutex<()>>>>,
    /// One cancellation sender per session that currently owns its execution
    /// lock. Queued turns do not replace the active sender.
    active_turns: Mutex<HashMap<SessionId, ActiveTurn>>,
    /// Tool invocation context (session identity + budget + surface).
    /// Used by system-driven ops like `remember` to attribute writes.
    tool_context: ToolContext,
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

struct RuntimeModels {
    available: HashMap<String, ModelInfo>,
    current_model: String,
    reasoning_effort: sylvander_protocol::ReasoningEffort,
}

#[derive(Debug, Clone, Copy, Default)]
struct ContextUsage {
    used_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
}

impl RuntimeModels {
    fn public_info(&self) -> sylvander_protocol::RuntimeModelInfo {
        let mut models = self
            .available
            .values()
            .map(|model| {
                let reasoning_efforts = if model
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
                    id: model.id.clone(),
                    provider: "anthropic-compatible".into(),
                    capabilities: model.capabilities.bits(),
                    reasoning_efforts,
                }
            })
            .collect::<Vec<_>>();
        models.sort_by(|left, right| left.id.cmp(&right.id));
        sylvander_protocol::RuntimeModelInfo {
            current_model: self.current_model.clone(),
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

impl AgentRun {
    /// Start building an [`AgentRun`].
    #[must_use]
    pub fn builder(spec: AgentSpec, client: AnthropicClient) -> AgentRunBuilder {
        AgentRunBuilder::new(spec, client)
    }

    /// Unique agent identifier.
    #[must_use]
    pub fn id(&self) -> &AgentId {
        &self.inner.id
    }

    /// Return the current tool invocation context (session identity,
    /// budget, surface). Used by system-driven operations like
    /// `remember` to attribute memory writes to the right identity.
    #[must_use]
    pub fn tool_context(&self) -> &ToolContext {
        &self.inner.tool_context
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
        let mut runtime = self.inner.runtime_models.write().await;
        let model = runtime
            .available
            .get(model_id)
            .ok_or_else(|| format!("model `{model_id}` is not available"))?;
        if reasoning_effort != sylvander_protocol::ReasoningEffort::Off
            && !model
                .capabilities
                .contains(ModelCapabilities::EXTENDED_THINKING)
        {
            return Err(format!(
                "model `{model_id}` does not support reasoning effort"
            ));
        }
        runtime.current_model = model_id.to_string();
        runtime.reasoning_effort = reasoning_effort;
        Ok(runtime.public_info())
    }

    pub async fn permission_profile(&self) -> sylvander_protocol::PermissionProfile {
        self.inner.runtime_permissions.read().await.clone()
    }

    pub async fn context_report(
        &self,
        session_id: Option<&SessionId>,
    ) -> sylvander_protocol::ContextReport {
        let models = self.inner.runtime_models.read().await;
        let model = models
            .available
            .get(&models.current_model)
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
            model: model.id.clone(),
            context_window: model.context_window,
            used_tokens: usage.used_tokens,
            remaining_tokens: model.context_window.saturating_sub(usage.used_tokens),
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
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
        if self
            .inner
            .active_turns
            .lock()
            .await
            .contains_key(session_id)
        {
            return Err("interrupt active work before compacting".into());
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
            return Err("interrupt active work before compacting".into());
        }
        let mut history = self
            .inner
            .sessions
            .read()
            .await
            .get(session_id)
            .ok_or_else(|| format!("unknown session: {session_id}"))?
            .history_snapshot();
        if history.len() <= 4 {
            return Err("not enough conversation history to compact".into());
        }
        let runtime = self.inner.runtime_models.read().await;
        let model = runtime
            .available
            .get(&runtime.current_model)
            .cloned()
            .expect("current model belongs to runtime catalog");
        drop(runtime);
        let usage = sylvander_llm_anthropic::api::types::Usage {
            input_tokens: model.context_window,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let summarizer =
            crate::compress::AgentLoopAutoCompactLlm::new(self.inner.loop_config.client.clone());
        let mut context = crate::compress::CompressContext {
            messages: &mut history,
            last_usage: &usage,
            model_info: &model,
            auto_compact_llm: Some(&summarizer),
        };
        let report = crate::compress::layers::auto_compact::AutoCompactLayer::new()
            .with_trigger_ratio(0.0)
            .apply(&mut context)
            .await;
        if let Some(reason) = report.failure.clone() {
            return Err(reason);
        }
        let layers = vec![report];
        self.inner
            .apply_compacted_history(session_id, &history, &layers)
            .await?;
        Ok(public_compaction_report(false, &layers))
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

    /// Join a session, creating a new [`SessionContext`].
    pub async fn join_session(&self, meta: SessionMetadata) -> SessionId {
        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let ctx = SessionContext::new(session_id.clone(), meta);
        self.inner
            .sessions
            .write()
            .await
            .insert(session_id.clone(), ctx);
        session_id
    }

    /// Leave a session.
    pub async fn leave_session(&self, session_id: &SessionId) {
        self.inner.sessions.write().await.remove(session_id);
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
    pub async fn run(self, mut inbox: mpsc::UnboundedReceiver<BusMessage>) {
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
                    } => {
                        let request = self.inner.pending_approvals.lock().await.remove(call_id);
                        if let Some(request) = request {
                            let decision = if *approved {
                                if !request.allowed_scopes.contains(scope) {
                                    crate::approval::ApprovalDecision::Rejected {
                                        reason: format!(
                                            "approval scope `{scope:?}` is not permitted"
                                        ),
                                    }
                                } else {
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
                                }
                            } else {
                                crate::approval::ApprovalDecision::Rejected {
                                    reason: "rejected by user".into(),
                                }
                            };
                            let _ = request.sender.send(decision);
                        }
                    }

                    // M18: forward AskUser answer to the waiting gate
                    SystemMessage::AnswerQuestion { call_id, answer } => {
                        let mut pending = self.inner.pending_answers.lock().await;
                        if let Some(request) = pending.remove(call_id) {
                            let _ = request.sender.send(vec![answer.clone()]);
                        }
                    }

                    SystemMessage::InterruptTurn { session_id } => {
                        self.inner.interrupt_turn(session_id).await;
                    }
                    SystemMessage::ResolvePlan { plan_id, decision } => {
                        let mut pending = self.inner.pending_plans.lock().await;
                        if let Some(request) = pending.remove(plan_id) {
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
                        {
                            if let Some(task) = tasks.remove(task_id) {
                                let _ = task.cancel.send(());
                            }
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
                        let result = inner.handle_message_interruptible(msg, interrupted).await;
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

    /// System-driven memory write (NOT a tool).
    pub async fn remember(
        &self,
        content: impl Into<String>,
        tags: &[&str],
    ) -> Result<MemoryEntry, MemoryStoreError> {
        let store = self
            .inner
            .memory
            .as_ref()
            .ok_or_else(|| MemoryStoreError::Store("no memory store configured".into()))?;
        let entry = MemoryEntry::new(
            uuid::Uuid::new_v4().to_string(),
            content,
            self.tool_context().session.as_ref().clone(),
        );
        let entry = tags.iter().fold(entry, |e, tag| e.with_tag(*tag, "true"));
        store
            .store(&self.tool_context().session, entry.clone())
            .await?;
        Ok(entry)
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
    pending_approvals: Arc<Mutex<HashMap<String, PendingApproval>>>,
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
                tool.call_id.clone(),
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
            let decision = match tokio::time::timeout(std::time::Duration::from_secs(120), rx).await
            {
                Ok(Ok(decision)) => decision,
                _ => ApprovalDecision::Rejected {
                    reason: "approval timeout".into(),
                },
            };
            decisions[index] = Some(decision);
            self.pending_approvals.lock().await.remove(&call_id);
        }
        ApprovalBatchResult {
            decisions: decisions
                .into_iter()
                .map(|decision| decision.expect("every approval decision must settle"))
                .collect(),
        }
    }
}

fn approval_fingerprint(tool: &ToolUseRequest) -> String {
    format!(
        "{}:{}",
        tool.tool_name,
        serde_json::to_string(&canonical_json(&tool.input)).unwrap_or_default()
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
    pending_answers: Arc<Mutex<HashMap<String, PendingAnswer>>>,
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
            call_id.to_string(),
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
        let answer = match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
            Ok(Ok(answer)) => answer,
            _ => Vec::new(),
        };
        self.pending_answers.lock().await.remove(call_id);
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
    pending_plans: Arc<Mutex<HashMap<String, PendingPlan>>>,
}

#[async_trait::async_trait]
impl PlanGate for BusPlanGate {
    async fn review(&self, plan_id: &str, steps: Vec<String>) -> crate::bus::PlanDecision {
        let (tx, rx) = oneshot::channel();
        self.pending_plans.lock().await.insert(
            plan_id.to_string(),
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

        let decision = match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
            Ok(Ok(decision)) => decision,
            _ => crate::bus::PlanDecision::Rejected {
                reason: "plan review timed out".into(),
            },
        };
        self.pending_plans.lock().await.remove(plan_id);
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
                let stored = StoredSession::new(
                    session_id.clone(),
                    metadata.name.clone(),
                    SessionLifetime::Persistent,
                    metadata.clone(),
                    vec![self.id.clone()],
                );
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
        self.handle_message_with_interrupt(msg, std::future::pending::<()>())
            .await
    }

    async fn handle_message_interruptible(
        &self,
        msg: BusMessage,
        interrupted: oneshot::Receiver<()>,
    ) -> Result<(), AgentRunError> {
        self.handle_message_with_interrupt(msg, interrupted).await
    }

    async fn handle_message_with_interrupt<F>(
        &self,
        msg: BusMessage,
        interrupted: F,
    ) -> Result<(), AgentRunError>
    where
        F: std::future::Future,
    {
        let session_id = msg.session_id.clone();
        let user_message = self.message_to_param(&msg);
        let (selected_model, selected_effort) = {
            let runtime = self.runtime_models.read().await;
            let model = runtime
                .available
                .get(&runtime.current_model)
                .expect("current runtime model must exist")
                .clone();
            (model, runtime.reasoning_effort)
        };

        // 1. Append user message + take history snapshot
        let (history, session_metadata) = {
            let mut sessions = self.sessions.write().await;
            let ctx = sessions
                .get_mut(&session_id)
                .ok_or_else(|| AgentRunError::UnknownSession(session_id.clone()))?;
            ctx.append_user_message(user_message.clone());
            (ctx.history_snapshot(), ctx.metadata.clone())
        };
        let permissions = self.runtime_permissions.read().await.clone();
        if let Some(store) = &self.session_store {
            let user_id = match &msg.sender {
                Sender::User(user_id) => user_id.as_str(),
                _ => "unix-client",
            };
            let caller = sylvander_protocol::SessionContext::new(
                user_id,
                self.id.clone(),
                session_id.clone(),
            );
            if let Ok(content) = serde_json::to_value(&user_message) {
                if let Err(error) = store
                    .append_message(
                        &caller,
                        &session_id,
                        StoredMessageRole::User,
                        content,
                        Some(&selected_model.id),
                        None,
                        None,
                    )
                    .await
                {
                    warn!(%session_id, %error, "failed to persist user message");
                }
            }
        }

        // 2. Build per-session approval gate and tool surface from one
        // permission snapshot. Changes made mid-turn apply to the next turn.
        let mut loop_config =
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
                let mut cfg = self.loop_config.clone();
                cfg.model = selected_model.clone();
                cfg.reasoning_effort = selected_effort;
                cfg.approval_gate = Some(gate);
                cfg
            } else {
                let mut cfg = self.loop_config.clone();
                cfg.model = selected_model.clone();
                cfg.reasoning_effort = selected_effort;
                cfg
            };
        if permissions.approval_policy == sylvander_protocol::ApprovalPolicy::Deny {
            loop_config.approval_gate = Some(Arc::new(DenyAllApprovalGate));
        }
        let tool_context = tool_context_for_permissions(
            &session_metadata,
            &self.id,
            &session_id,
            &permissions,
            self.memory.is_some(),
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
        let mut background_loop = self.loop_config.clone();
        background_loop.model = selected_model.clone();
        background_loop.reasoning_effort = selected_effort;
        background_loop.tool_context = tool_context;
        background_loop.tools = background_loop.tools.retain_named(&["read", "memory_read"]);
        background_loop.tool_definitions = background_loop.tools.definitions();
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
                crate::event::AgentEvent::IterationEnd { iteration, usage } => {
                    self.context_usage.write().await.insert(
                        session_id.clone(),
                        ContextUsage {
                            used_tokens: usage.total_input_tokens(),
                            cache_read_tokens: usage.cache_read_input_tokens.unwrap_or(0),
                            cache_write_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
                        },
                    );
                    let mut input_tokens = u64::from(usage.input_tokens);
                    let mut output_tokens = u64::from(usage.output_tokens);
                    if let Some(store) = &self.session_store {
                        match store
                            .record_usage(&session_id, usage.input_tokens, usage.output_tokens)
                            .await
                        {
                            Ok(total) => {
                                input_tokens = total.input_tokens;
                                output_tokens = total.output_tokens;
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
                crate::event::AgentEvent::Compressed { .. } => {}
                crate::event::AgentEvent::HistoryCompacted { layers, history } => {
                    let persisted = self
                        .apply_compacted_history(&session_id, &history, &layers)
                        .await;
                    if let Some(reason) = crate::compress::layer::first_failure(&layers) {
                        self.publish_stream(
                            &session_id,
                            crate::bus::StreamEvent::CompactionFailed {
                                automatic: true,
                                reason: reason.into(),
                            },
                        )
                        .await;
                    } else if let Err(reason) = persisted {
                        self.publish_stream(
                            &session_id,
                            crate::bus::StreamEvent::CompactionFailed {
                                automatic: true,
                                reason,
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
                crate::event::AgentEvent::AskUser {
                    call_id,
                    question,
                    options,
                    multi_select,
                } => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::AskUser {
                            call_id,
                            question,
                            options,
                            multi_select,
                        },
                    )
                    .await;
                }
                crate::event::AgentEvent::UserAnswer { call_id, answer } => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::UserAnswer { call_id, answer },
                    )
                    .await;
                }
                crate::event::AgentEvent::PlanProposed { .. }
                | crate::event::AgentEvent::PlanResolved { .. } => {}
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
                let user_id = self
                    .sessions
                    .read()
                    .await
                    .get(&session_id)
                    .map(|context| context.metadata.user_id.clone())
                    .unwrap_or_else(|| "unix-client".into());
                let caller = sylvander_protocol::SessionContext::new(
                    user_id,
                    self.id.clone(),
                    session_id.clone(),
                );
                let message = sylvander_llm_anthropic::api::types::MessageParam::assistant_blocks(
                    msg.content.clone(),
                );
                if let Ok(content) = serde_json::to_value(message) {
                    if let Err(error) = store
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

    fn message_to_param(
        &self,
        msg: &BusMessage,
    ) -> sylvander_llm_anthropic::api::types::MessageParam {
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

fn tool_context_for_permissions(
    metadata: &SessionMetadata,
    agent_id: &AgentId,
    session_id: &SessionId,
    permissions: &sylvander_protocol::PermissionProfile,
    memory_enabled: bool,
) -> ToolContext {
    let mut context = ToolContext::new(sylvander_protocol::SessionContext::new(
        metadata.user_id.clone(),
        agent_id.clone(),
        session_id.clone(),
    ))
    .with_fs_root(metadata.workspace.clone());
    match permissions.file_access {
        sylvander_protocol::FileAccess::None => {}
        sylvander_protocol::FileAccess::ReadOnly => {
            context = context.with_capability(Cap::Read);
        }
        sylvander_protocol::FileAccess::WorkspaceWrite => {
            context = context
                .with_capability(Cap::Read)
                .with_capability(Cap::Write);
        }
    }
    if permissions.network_access == sylvander_protocol::NetworkAccess::Allowed {
        context = context.with_capability(Cap::Network);
        context.surface.network = NetworkPolicy::All;
    }
    if memory_enabled {
        context = context
            .with_capability(Cap::MemoryRead)
            .with_capability(Cap::MemoryWrite);
    }
    context
}

// ---------------------------------------------------------------------------
// AgentRunBuilder
// ---------------------------------------------------------------------------

/// Builder for [`AgentRun`].
pub struct AgentRunBuilder {
    spec: AgentSpec,
    client: AnthropicClient,
    bus: Option<Arc<dyn MessageBus>>,
    tool_overrides: Option<ToolRegistry>,
    compression_overrides: Option<crate::compress::pipeline::CompressionPipeline>,
    memory: Option<Arc<dyn MemoryStore>>,
    session_store: Option<Arc<dyn SessionStore>>,
    model_capabilities: Option<sylvander_llm_anthropic::api::model::ModelCapabilities>,
    available_models: Vec<ModelInfo>,
    approval_enabled: bool,
    approval_rules: Vec<crate::approval::ApprovalRule>,
    approval_store_path: Option<PathBuf>,
}

impl AgentRunBuilder {
    fn new(spec: AgentSpec, client: AnthropicClient) -> Self {
        Self {
            spec,
            client,
            bus: None,
            tool_overrides: None,
            compression_overrides: None,
            memory: None,
            session_store: None,
            model_capabilities: None,
            available_models: Vec::new(),
            approval_enabled: false,
            approval_rules: Vec::new(),
            approval_store_path: None,
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

    pub fn override_compression(
        mut self,
        pipeline: crate::compress::pipeline::CompressionPipeline,
    ) -> Self {
        self.compression_overrides = Some(pipeline);
        self
    }

    /// Build the [`AgentRun`].
    pub fn build(self) -> Result<AgentRun, AgentRunError> {
        let id = self.spec.id.clone();
        let bus = self
            .bus
            .ok_or_else(|| AgentRunError::Build("bus is required".into()))?;

        let approval_memory = ApprovalMemory::load(self.approval_store_path.clone())?;
        let memory = if self.memory.is_some() {
            self.memory
        } else {
            self.spec
                .memory_stores
                .first()
                .and_then(|cfg| cfg.build().ok())
        };

        let mut model_info = self.spec.to_model_info();
        if let Some(caps) = self.model_capabilities {
            model_info.capabilities = caps;
        }
        let mut available_models = self
            .available_models
            .into_iter()
            .map(|model| (model.id.clone(), model))
            .collect::<HashMap<_, _>>();
        available_models
            .entry(model_info.id.clone())
            .or_insert_with(|| model_info.clone());
        let runtime_models = RuntimeModels {
            available: available_models,
            current_model: model_info.id.clone(),
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

        let mut loop_builder = AgentLoop::builder()
            .client(self.client)
            .model(model_info)
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

        // Clone the session for the run-level tool context before
        // moving `loop_config` into `AgentRunInner`.
        let run_tool_context = ToolContext::new(loop_config.tool_context.session.as_ref().clone());

        Ok(AgentRun {
            inner: Arc::new(AgentRunInner {
                id,
                spec: self.spec,
                loop_config,
                runtime_models: RwLock::new(runtime_models),
                runtime_permissions: RwLock::new(runtime_permissions),
                context_usage: RwLock::new(HashMap::new()),
                bus,
                sessions: RwLock::new(HashMap::new()),
                session_store: self.session_store,
                memory,
                approval_enabled: self.approval_enabled,
                approval_rules: self.approval_rules,
                pending_approvals: Arc::new(Mutex::new(HashMap::new())),
                approval_memory: Arc::new(Mutex::new(approval_memory)),
                pending_answers: Arc::new(Mutex::new(HashMap::new())),
                pending_plans: Arc::new(Mutex::new(HashMap::new())),
                background_tasks: Arc::new(Mutex::new(HashMap::new())),
                session_locks: Mutex::new(HashMap::new()),
                active_turns: Mutex::new(HashMap::new()),
                tool_context: run_tool_context,
            }),
        })
    }
}

// ---------------------------------------------------------------------------
// AgentRunError
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    #[error("unknown session: {0}")]
    UnknownSession(SessionId),
    #[error("loop error: {0}")]
    Loop(#[from] AgentLoopError),
    #[error("build error: {0}")]
    Build(String),
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
            .build()
            .expect("build");

        let initial = run.runtime_model_info().await;
        assert_eq!(initial.current_model, "claude-sonnet-5-20260601");
        assert_eq!(initial.models.len(), 2);
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
                used_tokens: 1_250,
                cache_read_tokens: 900,
                cache_write_tokens: 120,
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
            &metadata,
            &AgentId::new("agent"),
            &SessionId::new("session"),
            &sylvander_protocol::PermissionProfile {
                file_access: sylvander_protocol::FileAccess::ReadOnly,
                network_access: sylvander_protocol::NetworkAccess::Allowed,
                approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
            },
            true,
        );
        assert_eq!(
            context.surface.fs_root.as_deref(),
            Some(metadata.workspace.as_path())
        );
        assert!(context.has_cap(Cap::Read));
        assert!(!context.has_cap(Cap::Write));
        assert!(context.has_cap(Cap::Network));
        assert!(context.host_allowed("example.com"));
        assert!(context.has_cap(Cap::MemoryRead));
        assert_eq!(context.user_id().0, metadata.user_id);
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
    async fn remember_is_system_driven() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let store = Arc::new(InMemoryMemoryStore::new());
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .memory(store.clone())
            .build()
            .expect("build");
        run.remember("User prefers dark mode", &["preference"])
            .await
            .expect("remember");
        let results = store
            .search(
                &run.tool_context().session,
                "dark mode",
                crate::tools::memory::MemoryFilter::default(),
            )
            .await
            .expect("search");
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn remember_fails_without_memory_configured() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .build()
            .expect("build");
        let err = run.remember("something", &[]).await.unwrap_err();
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
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .build()
            .expect("build");
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
        let value = serde_json::to_value(run.inner.message_to_param(&message)).expect("json");
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
