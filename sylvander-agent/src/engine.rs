//! Agent run engine — lifecycle manager for agents and sessions.
//!
//! [`AgentRunEngine`] is the top-level orchestrator. All communication
//! flows through the message bus — there are no direct channels between
//! the engine and agents.
//!
//! # Communication model
//!
//! ```text
//! Engine                              Bus                         Agent
//!   │                                  │                            │
//!   │── publish(System::Stop) ────────►│                            │
//!   │                                  │── route ──────────────────►│
//!   │                                  │                            │ run() handles it
//!   │                                  │◄── StatusUpdate ──────────│
//!   │◄── status_rx ───────────────────│                            │
//! ```
//!
//! The engine subscribes to each agent's status updates so it can
//! detect dead/stuck agents.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{RwLock, mpsc};
use tracing::{info, warn};

use sylvander_llm_anthropic::api::client::AnthropicClient;

use crate::bus::{
    AgentStatus, BusMessage, MessageBus, MessageKind, Recipient, Sender, SystemMessage,
};
use crate::run::AgentRun;
use crate::session::SessionMetadata;
use crate::spec::{AgentId, AgentSpec, SessionId};

// ---------------------------------------------------------------------------
// AgentHandle
// ---------------------------------------------------------------------------

/// A handle to a spawned agent.
///
/// The engine uses this to monitor the agent's status and send
/// lifecycle commands (all via the bus).
#[derive(Debug)]
pub struct AgentHandle {
    /// Agent identifier.
    pub id: AgentId,
    /// The spec this agent was built from.
    pub spec: AgentSpec,
    /// Latest known status (updated from bus messages).
    pub status: AgentStatus,
    /// Receiver for the agent's status updates (engine monitors this).
    status_rx: mpsc::UnboundedReceiver<BusMessage>,
}

impl AgentHandle {
    /// Wait for the agent to reach a given status, with a timeout.
    ///
    /// Returns `true` if the status was reached, `false` on timeout.
    pub async fn wait_for_status(&mut self, target: AgentStatus, timeout_ms: u64) -> bool {
        if self.status == target {
            return true;
        }
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match tokio::time::timeout(remaining, self.status_rx.recv()).await {
                Ok(Some(msg)) => {
                    if let MessageKind::System(SystemMessage::StatusUpdate { status }) = msg.kind {
                        self.status = status;
                        if status == target {
                            return true;
                        }
                    }
                }
                _ => return false,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SessionMeta
// ---------------------------------------------------------------------------

/// Global metadata about a session.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    /// Session identifier.
    pub id: SessionId,
    /// Human-readable name.
    pub name: String,
    /// IDs of agents participating in this session.
    pub agents: Vec<AgentId>,
    /// Unix timestamp when the session was created.
    pub created_at: i64,
}

// ---------------------------------------------------------------------------
// AgentRunEngine
// ---------------------------------------------------------------------------

/// The top-level engine. All agent and session management flows
/// through the bus.
pub struct AgentRunEngine {
    /// Shared message bus.
    bus: Arc<dyn MessageBus>,
    /// Active agents (handle + status monitor).
    agents: RwLock<HashMap<AgentId, AgentHandle>>,
    /// Active sessions.
    sessions: RwLock<HashMap<SessionId, SessionMeta>>,
}

impl AgentRunEngine {
    /// Create a new engine.
    #[must_use]
    pub fn new(bus: Arc<dyn MessageBus>) -> Self {
        Self {
            bus,
            agents: RwLock::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Return a clone of the shared bus handle.
    #[must_use]
    pub fn bus(&self) -> Arc<dyn MessageBus> {
        self.bus.clone()
    }

    // -- agent lifecycle --

    /// Spawn a new agent.
    ///
    /// 1. Builds the AgentRun
    /// 2. Subscribes to the bus for the agent's messages
    /// 3. Subscribes to the bus for status updates
    /// 4. Spawns the tokio task
    ///
    /// # Errors
    /// Returns [`EngineError`] if the agent is already running.
    pub async fn spawn(
        &self,
        spec: AgentSpec,
        client: AnthropicClient,
    ) -> Result<AgentHandle, EngineError> {
        let agent_id = spec.id.clone();

        // Check for duplicates
        {
            let agents = self.agents.read().await;
            if agents.contains_key(&agent_id) {
                return Err(EngineError::AlreadySpawned(agent_id));
            }
        }

        // Build AgentRun
        let run = AgentRun::builder(spec.clone(), client)
            .bus(self.bus.clone())
            .build()
            .map_err(EngineError::Build)?;

        // Subscribe to all messages for this agent (chat + system)
        let filter = run.subscription_filter();
        let inbox = self
            .bus
            .subscribe(filter)
            .await
            .map_err(|e| EngineError::Bus(format!("agent subscribe failed: {e}")))?;

        // Subscribe to all broadcast messages — we filter for StatusUpdate
        // in the receiver. (We can't filter by SystemMessage variant alone
        // because PartialEq compares the inner fields too.)
        let status_filter = crate::bus::SubscriptionFilter {
            session_ids: None,
            recipients: Some(vec![Recipient::Broadcast]),
            kinds: None,
        };
        let status_rx = self
            .bus
            .subscribe(status_filter)
            .await
            .map_err(|e| EngineError::Bus(format!("status subscribe failed: {e}")))?;

        let agent_id_clone = agent_id.clone();

        // Spawn the agent task
        tokio::spawn(async move {
            run.run(inbox).await;
            info!(agent_id = %agent_id_clone, "agent task exited");
        });

        let handle = AgentHandle {
            id: agent_id.clone(),
            spec,
            status: AgentStatus::Starting,
            status_rx,
        };

        // Return a lightweight copy before moving the handle into the map
        let ret = AgentHandle {
            id: handle.id.clone(),
            spec: handle.spec.clone(),
            status: handle.status,
            status_rx: mpsc::unbounded_channel().1, // dummy — caller uses list_agents for status
        };

        self.agents.write().await.insert(agent_id.clone(), handle);

        info!(agent_id = %agent_id, "agent spawned");
        Ok(ret)
    }

    /// Despawn (stop) a running agent.
    ///
    /// Publishes a `System::Stop` message to the bus, then waits for
    /// the agent to report `Stopped`.
    ///
    /// # Errors
    /// Returns [`EngineError`] if the agent is not found or doesn't
    /// stop within the timeout.
    pub async fn despawn(&self, agent_id: &AgentId) -> Result<(), EngineError> {
        // Publish stop command via the bus
        let stop_msg = BusMessage::system_stop(agent_id.clone());
        self.bus
            .publish(stop_msg)
            .await
            .map_err(|e| EngineError::Bus(format!("stop publish failed: {e}")))?;

        // Wait for the agent to stop
        let stopped = {
            let mut agents = self.agents.write().await;
            if let Some(handle) = agents.get_mut(agent_id) {
                handle.wait_for_status(AgentStatus::Stopped, 5000).await
            } else {
                return Err(EngineError::NotFound(agent_id.clone()));
            }
        };

        if !stopped {
            warn!(agent_id = %agent_id, "agent did not stop within timeout");
        }

        self.agents.write().await.remove(agent_id);
        info!(agent_id = %agent_id, "agent despawned");
        Ok(())
    }

    /// List all running agents with their current status.
    pub async fn list_agents(&self) -> Vec<AgentHandle> {
        let mut agents = self.agents.write().await;
        let mut result = Vec::new();
        for handle in agents.values_mut() {
            // Drain pending status updates
            while let Ok(msg) = handle.status_rx.try_recv() {
                if let MessageKind::System(SystemMessage::StatusUpdate { status }) = msg.kind {
                    handle.status = status;
                }
            }
            result.push(AgentHandle {
                id: handle.id.clone(),
                spec: handle.spec.clone(),
                status: handle.status,
                status_rx: mpsc::unbounded_channel().1,
            });
        }
        result
    }

    /// Get a handle to a running agent.
    pub async fn get_agent(&self, agent_id: &AgentId) -> Option<AgentHandle> {
        let agents = self.agents.read().await;
        agents.get(agent_id).map(|h| AgentHandle {
            id: h.id.clone(),
            spec: h.spec.clone(),
            status: h.status,
            status_rx: mpsc::unbounded_channel().1,
        })
    }

    // -- session management --

    /// Create a new session and notify agents to join.
    ///
    /// Publishes `System::JoinSession` to each agent via the bus.
    /// The agents create their own `SessionContext` on receipt.
    pub async fn create_session(
        &self,
        name: impl Into<String>,
        metadata: SessionMetadata,
        agent_ids: &[AgentId],
    ) -> Result<SessionId, EngineError> {
        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let name = name.into();

        // Notify each agent to join via the bus
        for agent_id in agent_ids {
            let join_msg = BusMessage::system_join_session(
                agent_id.clone(),
                session_id.clone(),
                metadata.clone(),
            );
            self.bus
                .publish(join_msg)
                .await
                .map_err(|e| EngineError::Bus(format!("join publish failed: {e}")))?;
        }

        // Store session metadata
        let meta = SessionMeta {
            id: session_id.clone(),
            name,
            agents: agent_ids.to_vec(),
            created_at: crate::session::now_secs(),
        };
        self.sessions.write().await.insert(session_id.clone(), meta);

        info!(session_id = %session_id, "session created");
        Ok(session_id)
    }

    /// Send a user message to an agent in a session.
    pub async fn send_message(
        &self,
        session_id: SessionId,
        target: crate::bus::Recipient,
        text: impl Into<String>,
    ) -> Result<(), EngineError> {
        // Verify the session exists
        {
            let sessions = self.sessions.read().await;
            if !sessions.contains_key(&session_id) {
                return Err(EngineError::UnknownSession(session_id));
            }
        }

        let msg = BusMessage {
            session_id,
            sender: Sender::User("user".into()),
            recipient: target,
            kind: MessageKind::Chat,
            payload: text.into(),
            attachments: Vec::new(),
            timestamp: crate::session::now_secs(),
            id: crate::bus::MessageId::new(),
        };

        self.bus
            .publish(msg)
            .await
            .map_err(|e| EngineError::Bus(format!("publish failed: {e}")))?;

        Ok(())
    }

    /// List all sessions.
    pub async fn list_sessions(&self) -> Vec<SessionMeta> {
        self.sessions.read().await.values().cloned().collect()
    }

    /// Get metadata for a session.
    pub async fn get_session(&self, session_id: &SessionId) -> Option<SessionMeta> {
        self.sessions.read().await.get(session_id).cloned()
    }
}

// ---------------------------------------------------------------------------
// EngineError
// ---------------------------------------------------------------------------

/// Errors from engine operations.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Agent is already running.
    #[error("agent already spawned: {0}")]
    AlreadySpawned(AgentId),

    /// Agent not found.
    #[error("agent not found: {0}")]
    NotFound(AgentId),

    /// Session not found.
    #[error("unknown session: {0}")]
    UnknownSession(SessionId),

    /// AgentRun build failed.
    #[error("build error: {0}")]
    Build(#[from] crate::run::AgentRunError),

    /// Bus operation failed.
    #[error("bus error: {0}")]
    Bus(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{InProcessMessageBus, Recipient};

    fn test_spec(id: &str) -> AgentSpec {
        AgentSpec::builder()
            .id(id)
            .name(format!("Agent {id}"))
            .model_name("claude-sonnet-5-20260601")
            .build()
            .expect("spec build")
    }

    fn test_client() -> AnthropicClient {
        AnthropicClient::builder()
            .api_key("test-key")
            .build()
            .expect("client build")
    }

    #[tokio::test]
    async fn spawn_and_despawn() {
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = AgentRunEngine::new(bus);

        let handle = engine
            .spawn(test_spec("agent-1"), test_client())
            .await
            .expect("spawn");

        assert_eq!(handle.id, AgentId::new("agent-1"));
        assert_eq!(handle.status, AgentStatus::Starting);

        // Despawn via bus
        engine.despawn(&handle.id).await.expect("despawn");

        // Should be removed
        assert!(engine.get_agent(&AgentId::new("agent-1")).await.is_none());
    }

    #[tokio::test]
    async fn duplicate_spawn_is_error() {
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = AgentRunEngine::new(bus);

        engine
            .spawn(test_spec("dup-agent"), test_client())
            .await
            .expect("first spawn");

        let err = engine
            .spawn(test_spec("dup-agent"), test_client())
            .await
            .unwrap_err();

        assert!(matches!(err, EngineError::AlreadySpawned(_)));
    }

    #[tokio::test]
    async fn create_session_notifies_agents() {
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = AgentRunEngine::new(bus);

        // Spawn agent and drain its status updates so inbox is clean
        let _handle = engine
            .spawn(test_spec("agent-1"), test_client())
            .await
            .expect("spawn");

        // Give the agent a moment to start and publish status
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Now subscribe to the agent's inbox to observe JoinSession
        let mut observer = engine
            .bus()
            .subscribe(crate::bus::SubscriptionFilter::for_agent(AgentId::new(
                "agent-1",
            )))
            .await
            .expect("subscribe");

        // Drain any pending messages (status updates)
        while observer.try_recv().is_ok() {}

        let sid = engine
            .create_session(
                "test-session",
                SessionMetadata {
                    workspace: "/tmp".into(),
                    name: "test".into(),
                    user_id: "user-1".into(),
                },
                &[AgentId::new("agent-1")],
            )
            .await
            .expect("create_session");

        // Agent should receive JoinSession
        let msg = tokio::time::timeout(tokio::time::Duration::from_secs(2), observer.recv())
            .await
            .expect("timeout")
            .expect("should receive JoinSession");

        assert!(matches!(
            msg.kind,
            MessageKind::System(SystemMessage::JoinSession { .. })
        ));

        let meta = engine.get_session(&sid).await.expect("get_session");
        assert_eq!(meta.name, "test-session");

        // Clean up
        engine.despawn(&AgentId::new("agent-1")).await.ok();
    }

    #[tokio::test]
    async fn send_message_to_unknown_session_is_error() {
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = AgentRunEngine::new(bus);

        let err = engine
            .send_message(SessionId::new("nonexistent"), Recipient::Broadcast, "hello")
            .await
            .unwrap_err();

        assert!(matches!(err, EngineError::UnknownSession(_)));
    }

    #[tokio::test]
    async fn list_agents_and_sessions() {
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = AgentRunEngine::new(bus);

        engine
            .spawn(test_spec("agent-a"), test_client())
            .await
            .expect("spawn a");
        engine
            .spawn(test_spec("agent-b"), test_client())
            .await
            .expect("spawn b");

        // Let them start
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let agents = engine.list_agents().await;
        assert_eq!(agents.len(), 2);

        engine
            .create_session(
                "multi-agent",
                SessionMetadata {
                    workspace: "/tmp".into(),
                    name: "multi".into(),
                    user_id: "user-1".into(),
                },
                &[AgentId::new("agent-a"), AgentId::new("agent-b")],
            )
            .await
            .expect("create_session");

        assert_eq!(engine.list_sessions().await.len(), 1);

        // Clean up
        engine.despawn(&AgentId::new("agent-a")).await.ok();
        engine.despawn(&AgentId::new("agent-b")).await.ok();
    }
}
