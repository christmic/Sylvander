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

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{info, warn};

use sylvander_llm_anthropic::api::client::AnthropicClient;

use crate::approval::{ApprovalBatchResult, ApprovalDecision, ApprovalGate, ToolUseRequest};
use crate::bus::{
    AgentStatus as BusAgentStatus, BusMessage, MessageBus, MessageKind, Recipient, Sender,
    StreamEvent, SubscriptionFilter, SystemMessage, ToolCallInfo,
};
use crate::error::AgentLoopError;
use crate::loop_::{self, AgentLoop};
use crate::session::{now_secs, SessionContext, SessionMetadata};
use crate::spec::{AgentId, AgentSpec, SessionId};
use crate::tool::{Tool, ToolRegistry};
use crate::tools::memory::{MemoryEntry, MemoryStore, MemoryStoreError};
use crate::tools::MemoryReadTool;

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
    /// Handle to the message bus.
    bus: Arc<dyn MessageBus>,
    /// Per-session conversation state.
    sessions: RwLock<HashMap<SessionId, SessionContext>>,
    /// Long-term memory store.
    memory: Option<Arc<dyn MemoryStore>>,
    /// Whether bus-based approval is enabled (opt-in, off by default).
    approval_enabled: bool,
    /// Static approval rules (auto-approve/auto-reject).
    approval_rules: Vec<crate::approval::ApprovalRule>,
    /// Pending approval requests (shared with BusApprovalGate).
    pending_approvals: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<crate::approval::ApprovalDecision>>>>,
    /// Per-session concurrency locks (M12).
    session_locks: Mutex<HashMap<SessionId, Arc<Mutex<()>>>>,
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
        let _ = self.inner.bus.publish(BusMessage::system_status_update(
            self.inner.id.clone(),
            BusAgentStatus::Starting,
        )).await;
        let _ = self.inner.bus.publish(BusMessage::system_status_update(
            self.inner.id.clone(),
            BusAgentStatus::Running,
        )).await;

        while let Some(msg) = inbox.recv().await {
            match &msg.kind {
                // -- System messages --
                MessageKind::System(sys_msg) => match sys_msg {
                    SystemMessage::Stop => {
                        info!(agent_id = %self.inner.id, "received stop");
                        break;
                    }
                    SystemMessage::JoinSession { session_id, metadata } => {
                        let ctx = SessionContext::new(session_id.clone(), metadata.clone());
                        self.inner.sessions.write().await.insert(session_id.clone(), ctx);
                        info!(agent_id = %self.inner.id, %session_id, "joined session");
                    }
                    SystemMessage::LeaveSession { session_id } => {
                        self.inner.sessions.write().await.remove(session_id);
                        info!(agent_id = %self.inner.id, %session_id, "left session");
                    }
                    SystemMessage::StatusUpdate { .. } => {}

                    // M12: forward approval response to the waiting task
                    SystemMessage::ApproveTool { call_id, approved } => {
                        let mut pending = self.inner.pending_approvals.lock().await;
                        if let Some(tx) = pending.remove(call_id) {
                            let decision = if *approved {
                                crate::approval::ApprovalDecision::Approved
                            } else {
                                crate::approval::ApprovalDecision::Rejected {
                                    reason: "rejected by user".into(),
                                }
                            };
                            let _ = tx.send(decision);
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
                        if let Err(e) = inner.handle_message(msg).await {
                            warn!(error = %e, "handle_message failed");
                        }
                    });
                }

                // -- Stream events (for adapters) --
                MessageKind::Stream(_) => {}
            }
        }

        // Final status
        let _ = self.inner.bus.publish(BusMessage::system_status_update(
            self.inner.id.clone(),
            BusAgentStatus::Stopped,
        )).await;
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
        let store = self.inner.memory.as_ref().ok_or_else(|| {
            MemoryStoreError::Store("no memory store configured".into())
        })?;
        let entry = MemoryEntry::new(uuid::Uuid::new_v4().to_string(), content);
        let entry = tags.iter().fold(entry, |e, tag| e.with_tag(*tag, "true"));
        store.store(entry.clone()).await?;
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
    pending_approvals: Arc<
        Mutex<HashMap<String, tokio::sync::oneshot::Sender<ApprovalDecision>>>,
    >,
}

#[async_trait::async_trait]
impl ApprovalGate for BusApprovalGate {
    async fn check_batch(&self, tools: &[ToolUseRequest]) -> ApprovalBatchResult {
        let batch_id = uuid::Uuid::new_v4().to_string();
        let mut receivers: Vec<(String, tokio::sync::oneshot::Receiver<ApprovalDecision>)> =
            Vec::new();

        for tool in tools {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.pending_approvals
                .lock()
                .await
                .insert(tool.call_id.clone(), tx);
            receivers.push((tool.call_id.clone(), rx));
        }

        // Publish batch approval request
        let _ = self
            .bus
            .publish(BusMessage::stream_event(
                self.session_id.clone(),
                self.agent_id.clone(),
                StreamEvent::ToolApprovalRequired {
                    batch_id,
                    tools: tools
                        .iter()
                        .map(|t| ToolCallInfo {
                            call_id: t.call_id.clone(),
                            tool_name: t.tool_name.clone(),
                            input: t.input.clone(),
                        })
                        .collect(),
                },
            ))
            .await;

        // Wait for all decisions (120s timeout each)
        let mut decisions = Vec::new();
        for (_call_id, rx) in receivers {
            match tokio::time::timeout(std::time::Duration::from_secs(120), rx).await {
                Ok(Ok(d)) => decisions.push(d),
                _ => decisions.push(ApprovalDecision::Rejected {
                    reason: "approval timeout".into(),
                }),
            }
        }
        ApprovalBatchResult { decisions }
    }
}

// ---------------------------------------------------------------------------
// AgentRunInner — the actual implementation
// ---------------------------------------------------------------------------

impl AgentRunInner {
    /// Core: handle a chat message. Runs the loop with streaming.
    async fn handle_message(&self, msg: BusMessage) -> Result<(), AgentRunError> {
        let session_id = msg.session_id.clone();

        // 1. Append user message + take history snapshot
        let history = {
            let mut sessions = self.sessions.write().await;
            let ctx = sessions
                .get_mut(&session_id)
                .ok_or_else(|| AgentRunError::UnknownSession(session_id.clone()))?;
            ctx.append_user_message(self.message_to_param(&msg));
            ctx.history_snapshot()
        };

        // 2. Build per-session approval gate (if enabled)
        let loop_config = if self.approval_enabled {
            let bus_gate: Arc<dyn ApprovalGate> = Arc::new(BusApprovalGate {
                bus: self.bus.clone(),
                agent_id: self.id.clone(),
                session_id: session_id.clone(),
                pending_approvals: self.pending_approvals.clone(),
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
            cfg.approval_gate = Some(gate);
            cfg
        } else {
            self.loop_config.clone()
        };

        // 3. Run loop with streaming
        use futures_util::StreamExt;
        let mut stream = Box::pin(loop_::run_stream(&loop_config, history));
        let mut final_message: Option<sylvander_llm_anthropic::api::types::Message> = None;

        while let Some(event) = stream.next().await {
            match event {
                crate::event::AgentEvent::TextChunk(text) => {
                    self.publish_stream(&session_id, crate::bus::StreamEvent::TextDelta { delta: text }).await;
                }
                crate::event::AgentEvent::ThinkingChunk(text) => {
                    self.publish_stream(&session_id, crate::bus::StreamEvent::ThinkingDelta { delta: text }).await;
                }
                crate::event::AgentEvent::ToolCallStart { id, name, input } => {
                    self.publish_stream(&session_id, crate::bus::StreamEvent::ToolCall {
                        call_id: id, tool_name: name, input,
                    }).await;
                }
                crate::event::AgentEvent::ToolCallEnd { id, name, output, is_error } => {
                    self.publish_stream(&session_id, crate::bus::StreamEvent::ToolResult {
                        call_id: id, tool_name: name, output, is_error,
                    }).await;
                }
                crate::event::AgentEvent::ToolRejected { id, name, reason } => {
                    self.publish_stream(&session_id, crate::bus::StreamEvent::ToolResult {
                        call_id: id, tool_name: name, output: reason, is_error: true,
                    }).await;
                }
                crate::event::AgentEvent::IterationStart { iteration } => {
                    self.publish_stream(&session_id, crate::bus::StreamEvent::IterationStart { iteration }).await;
                }
                crate::event::AgentEvent::IterationEnd { iteration, usage } => {
                    self.publish_stream(&session_id, crate::bus::StreamEvent::IterationEnd {
                        iteration,
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                    }).await;
                }
                crate::event::AgentEvent::Compressed { .. } => {}
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
            self.publish_stream(&session_id, crate::bus::StreamEvent::Done { text }).await;
            let mut sessions = self.sessions.write().await;
            if let Some(ctx) = sessions.get_mut(&session_id) {
                ctx.append_assistant_message(msg);
            }
        }

        Ok(())
    }

    // -- helpers --

    async fn publish_stream(&self, session_id: &SessionId, event: crate::bus::StreamEvent) {
        let msg = BusMessage::stream_event(session_id.clone(), self.id.clone(), event);
        let _ = self.bus.publish(msg).await;
    }

    async fn publish_error(&self, session_id: &SessionId, err: &AgentLoopError) {
        let _ = self.bus.publish(BusMessage {
            session_id: session_id.clone(),
            sender: Sender::Agent(self.id.clone()),
            recipient: Recipient::Broadcast,
            kind: MessageKind::Chat,
            payload: format!("Error: {err}"),
            timestamp: now_secs(),
            id: crate::bus::MessageId::new(),
        }).await;
    }

    fn message_to_param(&self, msg: &BusMessage) -> sylvander_llm_anthropic::api::types::MessageParam {
        sylvander_llm_anthropic::api::types::MessageParam::user(&msg.payload)
    }
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
    model_capabilities: Option<sylvander_llm_anthropic::api::model::ModelCapabilities>,
    approval_enabled: bool,
    approval_rules: Vec<crate::approval::ApprovalRule>,
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
            model_capabilities: None,
            approval_enabled: false,
            approval_rules: Vec::new(),
        }
    }

    #[must_use]
    pub fn bus(mut self, bus: Arc<dyn MessageBus>) -> Self { self.bus = Some(bus); self }

    #[must_use]
    pub fn memory(mut self, store: Arc<dyn MemoryStore>) -> Self { self.memory = Some(store); self }

    #[must_use]
    pub fn override_tools(mut self, tools: ToolRegistry) -> Self { self.tool_overrides = Some(tools); self }

    #[must_use]
    pub fn model_capabilities(
        mut self, caps: sylvander_llm_anthropic::api::model::ModelCapabilities,
    ) -> Self { self.model_capabilities = Some(caps); self }

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

    pub fn override_compression(
        mut self, pipeline: crate::compress::pipeline::CompressionPipeline,
    ) -> Self { self.compression_overrides = Some(pipeline); self }

    /// Build the [`AgentRun`].
    pub fn build(self) -> Result<AgentRun, AgentRunError> {
        let id = self.spec.id.clone();
        let bus = self.bus.ok_or_else(|| AgentRunError::Build("bus is required".into()))?;

        let memory = if self.memory.is_some() {
            self.memory
        } else {
            self.spec.memory_stores.first().and_then(|cfg| cfg.build().ok())
        };

        let mut model_info = self.spec.to_model_info();
        if let Some(caps) = self.model_capabilities {
            model_info.capabilities = caps;
        }

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

        let loop_config = loop_builder.build()
            .map_err(|e| AgentRunError::Build(format!("loop build failed: {e}")))?;

        Ok(AgentRun {
            inner: Arc::new(AgentRunInner {
                id,
                spec: self.spec,
                loop_config,
                bus,
                sessions: RwLock::new(HashMap::new()),
                memory,
                approval_enabled: self.approval_enabled,
                approval_rules: self.approval_rules,
                pending_approvals: Arc::new(Mutex::new(HashMap::new())),
                session_locks: Mutex::new(HashMap::new()),
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
        SessionMetadata { workspace: PathBuf::from("/tmp/sylvander-test"), name: "test-session".into(), user_id: "user-1".into() }
    }

    fn test_spec_and_client() -> (AgentSpec, AnthropicClient) {
        let spec = AgentSpec::builder().id("test-agent").name("Test").model_name("claude-sonnet-5-20260601").build().expect("spec");
        let client = AnthropicClient::builder().api_key("test-key").build().expect("client");
        (spec, client)
    }

    #[tokio::test]
    async fn agent_run_is_cloneable() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let run = AgentRun::builder(spec, client).bus(bus).build().expect("build");
        let run2 = run.clone();
        assert_eq!(run.id(), run2.id());
    }

    #[tokio::test]
    async fn memory_is_infrastructure_not_tool() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let store = Arc::new(InMemoryMemoryStore::new());
        let run = AgentRun::builder(spec, client).bus(bus).memory(store).build().expect("build");
        let tools = run.memory_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "read_memory");
    }

    #[tokio::test]
    async fn remember_is_system_driven() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let store = Arc::new(InMemoryMemoryStore::new());
        let run = AgentRun::builder(spec, client).bus(bus).memory(store.clone()).build().expect("build");
        run.remember("User prefers dark mode", &["preference"]).await.expect("remember");
        let results = store.search("dark mode", 5).await.expect("search");
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn remember_fails_without_memory_configured() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let run = AgentRun::builder(spec, client).bus(bus).build().expect("build");
        let err = run.remember("something", &[]).await.unwrap_err();
        assert!(err.to_string().contains("no memory store"));
    }

    #[tokio::test]
    async fn memory_tools_empty_without_memory_configured() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let run = AgentRun::builder(spec, client).bus(bus).build().expect("build");
        assert!(run.memory_tools().is_empty());
    }

    #[tokio::test]
    async fn join_and_leave_session() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let run = AgentRun::builder(spec, client).bus(bus).build().expect("build");
        let sid = run.join_session(test_metadata()).await;
        assert_eq!(run.list_sessions().await.len(), 1);
        run.leave_session(&sid).await;
        assert!(run.list_sessions().await.is_empty());
    }

    #[tokio::test]
    async fn subscription_filter_matches_agent_and_broadcast() {
        let bus = Arc::new(InProcessMessageBus::new());
        let spec = AgentSpec::builder().id("filter-test").name("Filter Test").model_name("claude-sonnet-5-20260601").build().expect("spec");
        let client = AnthropicClient::builder().api_key("test-key").build().expect("client");
        let run = AgentRun::builder(spec, client).bus(bus.clone()).build().expect("build");
        let filter = run.subscription_filter();
        let agent_id = AgentId::new("filter-test");
        assert!(filter.matches(&BusMessage { recipient: Recipient::Agent(agent_id.clone()), ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi") }));
        assert!(filter.matches(&BusMessage { recipient: Recipient::Broadcast, ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi") }));
        assert!(!filter.matches(&BusMessage { recipient: Recipient::Agent(AgentId::new("other")), ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi") }));
    }
}
