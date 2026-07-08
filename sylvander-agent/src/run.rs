//! Agent runtime — the bridge between AgentLoop and the outside world.
//!
//! [`AgentRun`] is a running agent instance. It holds:
//! - Its [`AgentSpec`] (identity, personality, model, tools)
//! - An [`AgentLoop`] (the pure inference engine)
//! - A map of [`SessionContext`]s (one per session it participates in)
//! - A handle to the [`MessageBus`](crate::bus::MessageBus)
//! - A [`MemoryStore`](crate::tools::memory::MemoryStore) (infrastructure, not a tool)
//!
//! # Memory: mechanism first, tools second
//!
//! Memory is agent infrastructure — like the LLM client or tool registry.
//! The *read* path is exposed as a tool (`read_memory`) so the model can
//! autonomously retrieve relevant context. The *write* path is
//! system-driven — the agent cannot arbitrarily modify its own memory.
//! Writes happen via [`AgentRun::remember`] (called by the engine after
//! session milestones) or post-compression summarization.
//!
//! # Session: engineering layer, model-invisible
//!
//! Sessions are purely an engineering concern for message routing and
//! context isolation. The model never sees session IDs or session
//! management — it only receives the conversation history for the
//! current session.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use sylvander_llm_anthropic::api::client::AnthropicClient;

use crate::bus::{
    AgentStatus as BusAgentStatus, BusMessage, MessageBus, MessageKind, Recipient, Sender,
    SubscriptionFilter, SystemMessage,
};
use crate::error::AgentLoopError;
use crate::loop_::{self, AgentLoop};
use crate::session::{now_secs, SessionContext, SessionMetadata};
use crate::spec::{AgentId, AgentSpec, SessionId};
use crate::tool::{Tool, ToolRegistry};
use crate::tools::memory::{MemoryEntry, MemoryStore, MemoryStoreError};
use crate::tools::MemoryReadTool;

// ---------------------------------------------------------------------------
// AgentRun
// ---------------------------------------------------------------------------

/// A running agent instance.
///
/// Created via [`AgentRunBuilder`] and typically owned by an
/// [`AgentRunEngine`](crate::engine::AgentRunEngine).
pub struct AgentRun {
    /// Unique agent identifier (from the spec).
    pub id: AgentId,
    /// The spec this agent was built from (retained for engine introspection).
    #[allow(dead_code)]
    pub(crate) spec: AgentSpec,
    /// The pre-built loop configuration.
    pub(crate) loop_config: AgentLoop,
    /// Handle to the message bus.
    pub(crate) bus: Arc<dyn MessageBus>,
    /// Per-session conversation state.
    pub(crate) sessions: RwLock<HashMap<SessionId, SessionContext>>,
    /// Long-term memory store — agent infrastructure, not a tool.
    ///
    /// The model can *read* memory via the `read_memory` tool, but
    /// *writes* are system-driven (see [`Self::remember`]).
    pub(crate) memory: Option<Arc<dyn MemoryStore>>,
}

impl AgentRun {
    /// Start building an [`AgentRun`].
    #[must_use]
    pub fn builder(spec: AgentSpec, client: AnthropicClient) -> AgentRunBuilder {
        AgentRunBuilder::new(spec, client)
    }

    /// Return this agent's subscription filter.
    ///
    /// The agent subscribes to all messages addressed to it or
    /// broadcast, across all sessions. Session-level filtering is
    /// done inside [`handle_message`](Self::handle_message).
    #[must_use]
    pub fn subscription_filter(&self) -> SubscriptionFilter {
        SubscriptionFilter::for_agent(self.id.clone())
    }

    // -- session management --

    /// Join a session, creating a new [`SessionContext`].
    ///
    /// Returns the session ID for chaining.
    pub async fn join_session(&self, meta: SessionMetadata) -> SessionId {
        let session_id = SessionId::new(Uuid::new_v4().to_string());
        let ctx = SessionContext::new(session_id.clone(), meta);
        self.sessions.write().await.insert(session_id.clone(), ctx);
        session_id
    }

    /// Leave a session, discarding its context.
    pub async fn leave_session(&self, session_id: &SessionId) {
        self.sessions.write().await.remove(session_id);
    }

    /// List all sessions this agent is participating in.
    pub async fn list_sessions(&self) -> Vec<SessionId> {
        self.sessions.read().await.keys().cloned().collect()
    }

    /// Get a reference to a session context.
    pub async fn get_session(&self, session_id: &SessionId) -> Option<SessionContext> {
        self.sessions.read().await.get(session_id).cloned()
    }

    // -- message handling --

    /// Handle an incoming chat message: retrieve session context, run
    /// the loop with streaming, and publish every event to the bus.
    ///
    /// Streaming events ([`StreamEvent`]) are published in real-time:
    /// `TextDelta`, `ToolCall`, `ToolResult`, `IterationStart/End`,
    /// and `Done`. Only `Done` is written to session history — chunks
    /// are transient.
    ///
    /// # Errors
    /// Returns [`AgentRunError`] if the session is unknown or the
    /// loop fails.
    pub async fn handle_message(
        &self,
        msg: BusMessage,
    ) -> Result<(), AgentRunError> {
        let session_id = msg.session_id.clone();

        // 1. Append user message to session, then take history snapshot
        let history = {
            let mut sessions = self.sessions.write().await;
            let ctx = sessions
                .get_mut(&session_id)
                .ok_or_else(|| AgentRunError::UnknownSession(session_id.clone()))?;
            ctx.append_user_message(self.message_to_param(&msg));
            ctx.history_snapshot()
        };

        // 2. Run the loop with streaming — publish every event
        use futures_util::StreamExt;
        let mut stream = Box::pin(loop_::run_stream(&self.loop_config, history));
        let mut final_message: Option<
            sylvander_llm_anthropic::api::types::Message,
        > = None;

        while let Some(event) = stream.next().await {
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
                crate::event::AgentEvent::ToolCallStart { id, name, input } => {
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
                crate::event::AgentEvent::ToolCallEnd {
                    id,
                    name,
                    output,
                    is_error,
                } => {
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
                crate::event::AgentEvent::IterationStart { iteration } => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::IterationStart { iteration },
                    )
                    .await;
                }
                crate::event::AgentEvent::IterationEnd { iteration, usage } => {
                    self.publish_stream(
                        &session_id,
                        crate::bus::StreamEvent::IterationEnd {
                            iteration,
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                        },
                    )
                    .await;
                }
                crate::event::AgentEvent::Compressed { .. } => {
                    // Compression noise — not published to bus
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

        // 3. Write final message to session history + publish Done
        if let Some(msg) = final_message {
            let text = msg.text();
            self.publish_stream(
                &session_id,
                crate::bus::StreamEvent::Done { text },
            )
            .await;

            let mut sessions = self.sessions.write().await;
            if let Some(ctx) = sessions.get_mut(&session_id) {
                ctx.append_assistant_message(msg);
            }
        }

        Ok(())
    }

    /// Main event loop. Receives all messages from the bus — both chat
    /// and system messages — and dispatches them appropriately.
    ///
    /// System messages (Stop, JoinSession, LeaveSession) are handled
    /// directly; chat messages are passed to [`Self::handle_message`].
    ///
    /// Status updates are published to the bus on state transitions.
    pub async fn run(self, mut inbox: mpsc::UnboundedReceiver<BusMessage>) {
        // Publish initial status
        let _ = self
            .bus
            .publish(BusMessage::system_status_update(
                self.id.clone(),
                BusAgentStatus::Starting,
            ))
            .await;

        let _ = self
            .bus
            .publish(BusMessage::system_status_update(
                self.id.clone(),
                BusAgentStatus::Running,
            ))
            .await;

        while let Some(msg) = inbox.recv().await {
            match &msg.kind {
                // -- System messages (agent lifecycle) --
                MessageKind::System(sys_msg) => match sys_msg {
                    SystemMessage::Stop => {
                        info!(agent_id = %self.id, "received stop — shutting down");
                        break;
                    }

                    SystemMessage::JoinSession {
                        session_id,
                        metadata,
                    } => {
                        let ctx =
                            SessionContext::new(session_id.clone(), metadata.clone());
                        self.sessions
                            .write()
                            .await
                            .insert(session_id.clone(), ctx);
                        info!(
                            agent_id = %self.id,
                            session_id = %session_id,
                            "joined session"
                        );
                    }

                    SystemMessage::LeaveSession { session_id } => {
                        self.sessions.write().await.remove(session_id);
                        info!(
                            agent_id = %self.id,
                            session_id = %session_id,
                            "left session"
                        );
                    }

                    SystemMessage::StatusUpdate { .. } => {
                        // Status updates from other agents — ignore.
                        // We only publish, never consume these.
                    }
                },

                // -- Chat messages --
                MessageKind::Chat => {
                    let session_id = msg.session_id.clone();

                    {
                        let sessions = self.sessions.read().await;
                        if !sessions.contains_key(&session_id) {
                            warn!(
                                agent_id = %self.id,
                                session_id = %session_id,
                                "received chat for unknown session — ignoring"
                            );
                            continue;
                        }
                    }

                    match self.handle_message(msg).await {
                        Ok(_result) => {}
                        Err(err) => {
                            warn!(
                                agent_id = %self.id,
                                session_id = %session_id,
                                error = %err,
                                "agent loop failed"
                            );
                            let _ = self
                                .bus
                                .publish(BusMessage {
                                    session_id: session_id.clone(),
                                    sender: Sender::Agent(self.id.clone()),
                                    recipient: Recipient::Broadcast,
                                    kind: MessageKind::Chat,
                                    payload: format!("Error: {err}"),
                                    timestamp: now_secs(),
                                    id: crate::bus::MessageId::new(),
                                })
                                .await;
                        }
                    }
                }

                // -- Stream events (for adapters, agent ignores) --
                MessageKind::Stream(_) => {
                    // Stream events flow from agent to adapter — they are
                    // not consumed by the agent itself.
                }
            }
        }

        // Publish final status
        let _ = self
            .bus
            .publish(BusMessage::system_status_update(
                self.id.clone(),
                BusAgentStatus::Stopped,
            ))
            .await;

        info!(agent_id = %self.id, "agent loop exited");
    }

    // -- memory (infrastructure, not tools) --

    /// Return the tools that give the model access to this agent's memory.
    ///
    /// Currently returns only `read_memory` — the model can search but
    /// cannot write. Memory writes are system-driven via [`Self::remember`].
    #[must_use]
    pub fn memory_tools(&self) -> Vec<Arc<dyn Tool>> {
        match &self.memory {
            Some(store) => vec![Arc::new(MemoryReadTool::new(store.clone()))],
            None => vec![],
        }
    }

    /// Store a fact in the agent's long-term memory (system-driven).
    ///
    /// This is NOT exposed as a tool — the model cannot call it directly.
    /// The engine or session manager calls this after conversation
    /// milestones (session end, compression, explicit user "remember"
    /// command).
    ///
    /// # Errors
    /// Returns an error if no memory store is configured or the store
    /// operation fails.
    pub async fn remember(
        &self,
        content: impl Into<String>,
        tags: &[&str],
    ) -> Result<MemoryEntry, MemoryStoreError> {
        let store = self
            .memory
            .as_ref()
            .ok_or_else(|| MemoryStoreError::Store("no memory store configured".into()))?;

        let entry = MemoryEntry::new(uuid::Uuid::new_v4().to_string(), content);
        let entry = tags
            .iter()
            .fold(entry, |e, tag| e.with_tag(*tag, "true"));

        store.store(entry.clone()).await?;
        Ok(entry)
    }

    // -- streaming helpers --

    /// Publish a stream event to the bus (fire-and-forget).
    async fn publish_stream(&self, session_id: &SessionId, event: crate::bus::StreamEvent) {
        let msg = BusMessage::stream_event(session_id.clone(), self.id.clone(), event);
        let _ = self.bus.publish(msg).await;
    }

    /// Publish an error as a Chat message.
    async fn publish_error(&self, session_id: &SessionId, err: &AgentLoopError) {
        let _ = self
            .bus
            .publish(BusMessage {
                session_id: session_id.clone(),
                sender: Sender::Agent(self.id.clone()),
                recipient: Recipient::Broadcast,
                kind: MessageKind::Chat,
                payload: format!("Error: {err}"),
                timestamp: now_secs(),
                id: crate::bus::MessageId::new(),
            })
            .await;
    }

    // -- helpers --

    /// Convert a [`BusMessage`] to a [`MessageParam`] for the loop.
    fn message_to_param(&self, msg: &BusMessage) -> sylvander_llm_anthropic::api::types::MessageParam {
        sylvander_llm_anthropic::api::types::MessageParam::user(&msg.payload)
    }
}

// Need Uuid for SessionId generation
use uuid::Uuid;

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
        }
    }

    /// Set the message bus (required).
    #[must_use]
    pub fn bus(mut self, bus: Arc<dyn MessageBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Set the agent's memory store (infrastructure).
    ///
    /// If not set but the spec has `memory_stores` configured, the
    /// first compatible store is auto-resolved.
    #[must_use]
    pub fn memory(mut self, store: Arc<dyn MemoryStore>) -> Self {
        self.memory = Some(store);
        self
    }

    /// Override the tool registry (otherwise empty).
    ///
    /// Callers should include [`AgentRun::memory_tools`] in the
    /// registry if they want the model to have memory access.
    #[must_use]
    pub fn override_tools(mut self, tools: ToolRegistry) -> Self {
        self.tool_overrides = Some(tools);
        self
    }

    /// Add model capabilities (e.g. `TOOL_USE`).
    ///
    /// The spec's `ModelConfig` does not encode capabilities, so
    /// callers must set them explicitly when registering tools.
    #[must_use]
    pub fn model_capabilities(
        mut self,
        caps: sylvander_llm_anthropic::api::model::ModelCapabilities,
    ) -> Self {
        self.model_capabilities = Some(caps);
        self
    }

    /// Override the compression pipeline (otherwise default).
    #[must_use]
    pub fn override_compression(
        mut self,
        pipeline: crate::compress::pipeline::CompressionPipeline,
    ) -> Self {
        self.compression_overrides = Some(pipeline);
        self
    }

    /// Build the [`AgentRun`].
    ///
    /// Memory stores from the spec are auto-resolved if no explicit
    /// store was set via [`Self::memory`].
    ///
    /// # Errors
    /// Returns [`AgentRunError`] if required fields are missing or
    /// the loop configuration fails.
    pub fn build(self) -> Result<AgentRun, AgentRunError> {
        let id = self.spec.id.clone();
        let bus = self
            .bus
            .ok_or_else(|| AgentRunError::Build("bus is required".into()))?;

        // Resolve memory store: explicit override > spec config > None
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

        Ok(AgentRun {
            id,
            spec: self.spec,
            loop_config,
            bus,
            sessions: RwLock::new(HashMap::new()),
            memory,
        })
    }
}

// ---------------------------------------------------------------------------
// AgentRunError
// ---------------------------------------------------------------------------

/// Errors from [`AgentRun`] operations.
#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    /// The message referenced an unknown session.
    #[error("unknown session: {0}")]
    UnknownSession(SessionId),

    /// The underlying loop failed.
    #[error("loop error: {0}")]
    Loop(#[from] AgentLoopError),

    /// Builder configuration error.
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
    async fn memory_is_infrastructure_not_tool() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();
        let store = Arc::new(InMemoryMemoryStore::new());

        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .memory(store)
            .build()
            .expect("build");

        // Memory tools return only read_memory (not write_memory)
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

        // System-driven write (not a tool!)
        run.remember("User prefers dark mode", &["preference"])
            .await
            .expect("remember");

        // Verify it was stored
        let results = store.search("dark mode", 5).await.expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "User prefers dark mode");
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

    #[tokio::test]
    async fn join_and_leave_session() {
        let bus = Arc::new(InProcessMessageBus::new());
        let (spec, client) = test_spec_and_client();

        let run = AgentRun::builder(spec, client)
            .bus(bus)
            .build()
            .expect("build");

        // Join a session
        let sid = run.join_session(test_metadata()).await;
        assert_eq!(run.list_sessions().await.len(), 1);
        assert!(run.get_session(&sid).await.is_some());

        // Leave the session
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

        let run = AgentRun::builder(spec, client)
            .bus(bus.clone())
            .build()
            .expect("build");

        let filter = run.subscription_filter();

        let agent_id = AgentId::new("filter-test");

        // Should match: directed to this agent
        assert!(filter.matches(&BusMessage {
            recipient: Recipient::Agent(agent_id.clone()),
            ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi")
        }));

        // Should match: broadcast
        assert!(filter.matches(&BusMessage {
            recipient: Recipient::Broadcast,
            ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi")
        }));

        // Should NOT match: directed to another agent
        assert!(!filter.matches(&BusMessage {
            recipient: Recipient::Agent(AgentId::new("other-agent")),
            ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi")
        }));
    }
}
