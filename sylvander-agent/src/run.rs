//! Agent runtime — the bridge between AgentLoop and the outside world.
//!
//! [`AgentRun`] is a running agent instance. It holds:
//! - Its [`AgentSpec`] (identity, personality, model, tools)
//! - An [`AgentLoop`] (the pure inference engine)
//! - A map of [`SessionContext`]s (one per session it participates in)
//! - A handle to the [`MessageBus`](crate::bus::MessageBus)
//!
//! [`AgentRun`] does NOT own its lifecycle — that's the job of
//! [`AgentRunEngine`](crate::engine::AgentRunEngine) (M6).
//!
//! # Agent : Session = N : N
//!
//! An agent can participate in multiple sessions simultaneously. Each
//! session has its own isolated conversation history.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use tracing::warn;

use sylvander_llm_anthropic::api::client::AnthropicClient;

use crate::bus::{BusMessage, MessageBus, MessageKind, Recipient, Sender, SubscriptionFilter};
use crate::error::AgentLoopError;
use crate::loop_::{self, AgentLoop, AgentLoopResult};
use crate::session::{now_secs, SessionContext, SessionMetadata};
use crate::spec::{AgentId, AgentSpec, SessionId};
use crate::tool::ToolRegistry;

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

    /// Handle an incoming message: retrieve session context, run the
    /// loop, update history, and publish a response.
    ///
    /// This is the core method that bridges the message bus and the
    /// inference engine.
    ///
    /// # Errors
    /// Returns [`AgentRunError`] if the session is unknown or the
    /// loop fails.
    pub async fn handle_message(
        &self,
        msg: BusMessage,
    ) -> Result<AgentLoopResult, AgentRunError> {
        let session_id = msg.session_id.clone();

        // 1. Retrieve session context
        let sessions = self.sessions.read().await;
        let ctx = sessions
            .get(&session_id)
            .ok_or_else(|| AgentRunError::UnknownSession(session_id.clone()))?;

        // 2. Take a snapshot + append the incoming message
        let mut history = ctx.history_snapshot();
        drop(sessions);

        history.push(self.message_to_param(&msg));

        // 3. Run the loop (pure engine, no session awareness)
        let result = loop_::run(&self.loop_config, history)
            .await
            .map_err(AgentRunError::Loop)?;

        // 4. Write the assistant response back to session history
        {
            let mut sessions = self.sessions.write().await;
            if let Some(ctx) = sessions.get_mut(&session_id) {
                ctx.append_assistant_message(result.final_message.clone());
            }
        }

        // 5. Publish the response to the bus
        let response_text = result
            .final_message
            .content
            .iter()
            .filter_map(|block| match block {
                sylvander_llm_anthropic::api::types::ContentBlock::Text(t) => {
                    Some(t.text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let response = BusMessage::agent_response(session_id, self.id.clone(), response_text);
        let _ = self.bus.publish(response).await;

        Ok(result)
    }

    /// Main event loop. Consumes messages from the inbox and processes
    /// them one at a time.
    ///
    /// This is designed to be spawned as a tokio task by the engine.
    pub async fn run(self, mut inbox: mpsc::UnboundedReceiver<BusMessage>) {
        while let Some(msg) = inbox.recv().await {
            let session_id = msg.session_id.clone();

            // Check that we're actually in this session
            {
                let sessions = self.sessions.read().await;
                if !sessions.contains_key(&session_id) {
                    warn!(
                        agent_id = %self.id,
                        session_id = %session_id,
                        "received message for unknown session — ignoring"
                    );
                    continue;
                }
            }

            match self.handle_message(msg).await {
                Ok(_result) => {
                    // Loop completed successfully
                }
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
}

impl AgentRunBuilder {
    fn new(spec: AgentSpec, client: AnthropicClient) -> Self {
        Self {
            spec,
            client,
            bus: None,
            tool_overrides: None,
            compression_overrides: None,
        }
    }

    /// Set the message bus (required).
    #[must_use]
    pub fn bus(mut self, bus: Arc<dyn MessageBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Override the tool registry (otherwise empty).
    #[must_use]
    pub fn override_tools(mut self, tools: ToolRegistry) -> Self {
        self.tool_overrides = Some(tools);
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
    /// # Errors
    /// Returns [`AgentRunError`] if required fields are missing or
    /// the loop configuration fails.
    pub fn build(self) -> Result<AgentRun, AgentRunError> {
        let id = self.spec.id.clone();
        let bus = self
            .bus
            .ok_or_else(|| AgentRunError::Build("bus is required".into()))?;

        let model_info = self.spec.to_model_info();

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
    use std::path::PathBuf;

    fn test_metadata() -> SessionMetadata {
        SessionMetadata {
            workspace: PathBuf::from("/tmp/sylvander-test"),
            name: "test-session".into(),
            user_id: "user-1".into(),
        }
    }

    #[tokio::test]
    async fn join_and_leave_session() {
        let bus = Arc::new(InProcessMessageBus::new());
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
