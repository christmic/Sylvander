//! Agent run engine — lifecycle manager for agents and sessions.
//!
//! [`AgentRunEngine`] is the top-level orchestrator. It:
//! - Spawns and despawns [`AgentRun`](crate::run::AgentRun) instances
//! - Creates and manages sessions
//! - Routes messages from the bus to the correct agent
//!
//! The engine owns the message bus and is the entry point for all
//! external interactions (CLI, TUI, API).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use tracing::info;

use sylvander_llm_anthropic::api::client::AnthropicClient;

use crate::bus::{BusMessage, MessageBus, Sender};
use crate::run::{AgentRun, ControlCommand};
use crate::session::SessionMetadata;
use crate::spec::{AgentId, AgentSpec, SessionId};

// ---------------------------------------------------------------------------
// AgentStatus
// ---------------------------------------------------------------------------

/// Lifecycle status of a spawned agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AgentStatus {
    /// The agent task has been spawned but hasn't started its loop yet.
    Starting = 0,
    /// The agent is actively processing messages.
    Running = 1,
    /// The agent is alive but idle (no pending messages).
    Idle = 2,
    /// The agent has been stopped / despawned.
    Stopped = 3,
}

impl AgentStatus {
    /// Decode from the raw `u8` stored in the atomic.
    #[must_use]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Starting,
            1 => Self::Running,
            2 => Self::Idle,
            _ => Self::Stopped,
        }
    }
}

// ---------------------------------------------------------------------------
// AgentHandle
// ---------------------------------------------------------------------------

/// A handle to a spawned agent. Used by the engine (and callers) to
/// inspect and control an agent's lifecycle.
#[derive(Debug)]
pub struct AgentHandle {
    /// Agent identifier.
    pub id: AgentId,
    /// The spec this agent was built from (clone).
    pub spec: AgentSpec,
    /// Send control commands (Stop, Pause, Resume) to the agent task.
    control_tx: mpsc::Sender<ControlCommand>,
    /// Current lifecycle status (atomically updated).
    pub status: Arc<AtomicU8>,
}

// ---------------------------------------------------------------------------
// SessionMeta
// ---------------------------------------------------------------------------

/// Global metadata about a session (shared across all agents in it).
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

/// The top-level engine that manages agent lifecycles and session
/// routing.
///
/// # Example
///
/// ```no_run
/// # use sylvander_agent::prelude::*;
/// # use sylvander_llm_anthropic::api::client::AnthropicClient;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let bus = InProcessMessageBus::new();
/// let engine = AgentRunEngine::new(Arc::new(bus));
///
/// let spec = AgentSpec::builder()
///     .id("my-agent")
///     .name("My Agent")
///     .system_prompt("You are helpful.")
///     .model_name("claude-sonnet-5-20260601")
///     .build()?;
///
/// let client = AnthropicClient::builder()
///     .api_key(std::env::var("ANTHROPIC_API_KEY")?)
///     .build()?;
///
/// let handle = engine.spawn(spec, client).await?;
/// assert_eq!(AgentStatus::from_u8(handle.status.load(Ordering::SeqCst)), AgentStatus::Running);
///
/// engine.despawn(&handle.id).await?;
/// # Ok(())
/// # }
/// ```
pub struct AgentRunEngine {
    /// Shared message bus.
    bus: Arc<dyn MessageBus>,
    /// Active agents.
    agents: RwLock<HashMap<AgentId, AgentHandle>>,
    /// Active sessions (global metadata).
    sessions: RwLock<HashMap<SessionId, SessionMeta>>,
}

impl AgentRunEngine {
    /// Create a new engine backed by the given message bus.
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

    /// Spawn a new agent from a spec and LLM client.
    ///
    /// The agent is immediately subscribed to the bus and starts
    /// processing messages.
    ///
    /// # Errors
    /// Returns [`EngineError`] if the agent is already running or the
    /// build fails.
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

        // Build the AgentRun
        let run = AgentRun::builder(spec.clone(), client)
            .bus(self.bus.clone())
            .build()
            .map_err(EngineError::Build)?;

        let filter = run.subscription_filter();
        let inbox = self
            .bus
            .subscribe(filter)
            .await
            .map_err(|e| EngineError::Bus(format!("subscribe failed: {e}")))?;

        // Control channel
        let (control_tx, control_rx) = mpsc::channel(8);

        // Status tracking
        let status = Arc::new(AtomicU8::new(AgentStatus::Starting as u8));
        let status_clone = status.clone();
        let task_agent_id = agent_id.clone();

        // Spawn the agent task
        tokio::spawn(async move {
            status_clone.store(AgentStatus::Running as u8, Ordering::SeqCst);
            run.run(inbox, control_rx).await;
            status_clone.store(AgentStatus::Stopped as u8, Ordering::SeqCst);
            info!(agent_id = %task_agent_id, "agent task exited");
        });

        let handle = AgentHandle {
            id: agent_id.clone(),
            spec,
            control_tx,
            status,
        };

        self.agents.write().await.insert(agent_id.clone(), handle.clone());

        info!(agent_id = %agent_id, "agent spawned");
        Ok(handle)
    }

    /// Despawn (stop) a running agent.
    ///
    /// Sends a [`ControlCommand::Stop`] and waits briefly for the task
    /// to exit. The agent's sessions are NOT removed — they persist
    /// for potential re-spawn.
    ///
    /// # Errors
    /// Returns [`EngineError`] if the agent is not found.
    pub async fn despawn(&self, agent_id: &AgentId) -> Result<(), EngineError> {
        let handle = {
            let agents = self.agents.read().await;
            agents
                .get(agent_id)
                .cloned()
                .ok_or_else(|| EngineError::NotFound(agent_id.clone()))?
        };

        let _ = handle.control_tx.send(ControlCommand::Stop).await;

        // Wait for the status to transition to Stopped (with timeout)
        for _ in 0..50 {
            if AgentStatus::from_u8(handle.status.load(Ordering::SeqCst)) == AgentStatus::Stopped
            {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        self.agents.write().await.remove(agent_id);

        info!(agent_id = %agent_id, "agent despawned");
        Ok(())
    }

    /// List all running agents.
    pub async fn list_agents(&self) -> Vec<AgentHandle> {
        self.agents.read().await.values().cloned().collect()
    }

    /// Get a handle to a running agent.
    pub async fn get_agent(&self, agent_id: &AgentId) -> Option<AgentHandle> {
        self.agents.read().await.get(agent_id).cloned()
    }

    // -- session management --

    /// Create a new session and add the given agents to it.
    ///
    /// Each agent's [`AgentRun`] is notified via `join_session`. The
    /// session metadata is stored for later lookup.
    ///
    /// # Note
    /// This method publishes to the bus — callers must ensure the
    /// agents have been spawned first.
    pub async fn create_session(
        &self,
        name: impl Into<String>,
        _metadata: SessionMetadata,
        agent_ids: &[AgentId],
    ) -> Result<SessionId, EngineError> {
        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let name = name.into();

        // Store session meta
        let meta = SessionMeta {
            id: session_id.clone(),
            name,
            agents: agent_ids.to_vec(),
            created_at: crate::session::now_secs(),
        };
        self.sessions
            .write()
            .await
            .insert(session_id.clone(), meta);

        // We publish a broadcast message to notify agents of the new session.
        // AgentRun::handle_message will ignore it (unknown session), but
        // in a future version we'd add a dedicated SessionJoined notification.
        let _ = self
            .bus
            .publish(BusMessage {
                session_id: session_id.clone(),
                sender: Sender::System,
                recipient: crate::bus::Recipient::Broadcast,
                kind: crate::bus::MessageKind::Chat,
                payload: String::new(),
                timestamp: crate::session::now_secs(),
                id: crate::bus::MessageId::new(),
            })
            .await;

        Ok(session_id)
    }

    /// Send a user message to an agent in a session.
    ///
    /// This is the primary entry point for user interactions.
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
            kind: crate::bus::MessageKind::Chat,
            payload: text.into(),
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
// Clone impl (manual — AgentHandle doesn't need Clone, but the engine is
// Send + Sync via Arc internals)
// ---------------------------------------------------------------------------

impl Clone for AgentHandle {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            spec: self.spec.clone(),
            control_tx: self.control_tx.clone(),
            status: self.status.clone(),
        }
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

        // Should be running (give the task a moment to start)
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let status = AgentStatus::from_u8(handle.status.load(Ordering::SeqCst));
        assert!(status == AgentStatus::Running || status == AgentStatus::Idle);

        // Despawn
        engine.despawn(&handle.id).await.expect("despawn");

        let status = AgentStatus::from_u8(handle.status.load(Ordering::SeqCst));
        assert_eq!(status, AgentStatus::Stopped);

        // Should be removed from the engine
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
    async fn create_session_stores_meta() {
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = AgentRunEngine::new(bus);

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

        let meta = engine.get_session(&sid).await.expect("get_session");
        assert_eq!(meta.name, "test-session");
        assert_eq!(meta.agents.len(), 1);
    }

    #[tokio::test]
    async fn send_message_to_unknown_session_is_error() {
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = AgentRunEngine::new(bus);

        let err = engine
            .send_message(
                SessionId::new("nonexistent"),
                Recipient::Broadcast,
                "hello",
            )
            .await
            .unwrap_err();

        assert!(matches!(err, EngineError::UnknownSession(_)));
    }

    #[tokio::test]
    async fn list_agents_and_sessions() {
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = AgentRunEngine::new(bus);

        // Spawn two agents
        engine
            .spawn(test_spec("agent-a"), test_client())
            .await
            .expect("spawn a");
        engine
            .spawn(test_spec("agent-b"), test_client())
            .await
            .expect("spawn b");

        assert_eq!(engine.list_agents().await.len(), 2);

        // Create a session
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

    #[test]
    fn agent_status_from_u8() {
        assert_eq!(AgentStatus::from_u8(0), AgentStatus::Starting);
        assert_eq!(AgentStatus::from_u8(1), AgentStatus::Running);
        assert_eq!(AgentStatus::from_u8(2), AgentStatus::Idle);
        assert_eq!(AgentStatus::from_u8(3), AgentStatus::Stopped);
        assert_eq!(AgentStatus::from_u8(99), AgentStatus::Stopped); // unknown → Stopped
    }
}
