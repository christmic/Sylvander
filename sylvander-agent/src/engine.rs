//! Agent run engine — lifecycle manager for agents and sessions.
//!
//! [`AgentRunEngine`](crate::engine::AgentRunEngine) is the top-level orchestrator. All communication
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
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::bus::{
    AgentStatus, BusMessage, MessageBus, MessageKind, Recipient, Sender, SystemMessage,
};
use crate::run::AgentRun;
use crate::session::SessionMetadata;
use crate::spec::{AgentId, AgentSpec, SessionId};

/// Supplies immutable Agent runs to the engine's revision router.
///
/// The engine deliberately knows nothing about registries or configuration.
/// A runtime resolves each durable session binding and composes the requested
/// revision on demand.
#[async_trait::async_trait]
pub trait RevisionedAgentRunProvider: Send + Sync {
    /// Return the immutable Agent revision bound to `session_id`.
    async fn revision_for_session(
        &self,
        agent_id: &AgentId,
        session_id: &SessionId,
    ) -> Result<u64, String>;

    /// Build or retrieve the run for one immutable revision.
    async fn run_for_revision(&self, agent_id: &AgentId, revision: u64)
    -> Result<AgentRun, String>;
}

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
    status_rx: mpsc::Receiver<BusMessage>,
    task: Option<JoinHandle<()>>,
    expected_exit: Arc<AtomicBool>,
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
    exit_tx: mpsc::UnboundedSender<AgentId>,
    exits: tokio::sync::Mutex<mpsc::UnboundedReceiver<AgentId>>,
}

struct AgentExitSignal {
    agent_id: AgentId,
    expected: Arc<AtomicBool>,
    sender: mpsc::UnboundedSender<AgentId>,
}

impl Drop for AgentExitSignal {
    fn drop(&mut self) {
        if !self.expected.load(Ordering::SeqCst) {
            let _ = self.sender.send(self.agent_id.clone());
        }
    }
}

impl AgentRunEngine {
    /// Create a new engine.
    #[must_use]
    pub fn new(bus: Arc<dyn MessageBus>) -> Self {
        let (exit_tx, exits) = mpsc::unbounded_channel();
        Self {
            bus,
            agents: RwLock::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
            exit_tx,
            exits: tokio::sync::Mutex::new(exits),
        }
    }

    /// Return a clone of the shared bus handle.
    #[must_use]
    pub fn bus(&self) -> Arc<dyn MessageBus> {
        self.bus.clone()
    }

    // -- agent lifecycle --

    /// Spawn a fully configured Agent run.
    ///
    /// This is the composition-root entry point for runtimes that provide
    /// durable stores, tools, model catalogs, and approval policy.
    pub async fn spawn_run(
        &self,
        spec: AgentSpec,
        run: AgentRun,
    ) -> Result<AgentHandle, EngineError> {
        let agent_id = spec.id.clone();
        if run.id() != &agent_id {
            return Err(EngineError::AgentRunMismatch {
                spec: agent_id,
                run: run.id().clone(),
            });
        }

        let mut agents = self.agents.write().await;
        if agents.contains_key(&agent_id) {
            return Err(EngineError::AlreadySpawned(agent_id));
        }

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
        let expected_exit = Arc::new(AtomicBool::new(false));
        let task_expected_exit = expected_exit.clone();
        let exit_tx = self.exit_tx.clone();

        // Spawn the agent task
        let task = tokio::spawn(async move {
            let _exit_signal = AgentExitSignal {
                agent_id: agent_id_clone.clone(),
                expected: task_expected_exit,
                sender: exit_tx,
            };
            run.run(inbox).await;
            info!(agent_id = %agent_id_clone, "agent task exited");
        });

        let handle = AgentHandle {
            id: agent_id.clone(),
            spec,
            status: AgentStatus::Starting,
            status_rx,
            task: Some(task),
            expected_exit: expected_exit.clone(),
        };

        // Return a lightweight copy before moving the handle into the map
        let ret = AgentHandle {
            id: handle.id.clone(),
            spec: handle.spec.clone(),
            status: handle.status,
            status_rx: mpsc::channel(1).1, // dummy — caller uses list_agents for status
            task: None,
            expected_exit,
        };

        agents.insert(agent_id.clone(), handle);

        info!(agent_id = %agent_id, "agent spawned");
        Ok(ret)
    }

    /// Spawn one logical Agent whose sessions may be pinned to different
    /// immutable revisions.
    ///
    /// There remains exactly one canonical bus subscription and one engine
    /// handle for the Agent. Messages are routed to revision-specific
    /// [`AgentRun`] workers according to the provider's durable session
    /// binding. Existing sessions therefore cannot drift when another
    /// revision becomes active for newly created sessions.
    pub async fn spawn_revisioned_run(
        &self,
        spec: AgentSpec,
        initial_revision: u64,
        initial_run: AgentRun,
        provider: Arc<dyn RevisionedAgentRunProvider>,
    ) -> Result<AgentHandle, EngineError> {
        let agent_id = spec.id.clone();
        if initial_run.id() != &agent_id {
            return Err(EngineError::AgentRunMismatch {
                spec: agent_id,
                run: initial_run.id().clone(),
            });
        }

        let mut agents = self.agents.write().await;
        if agents.contains_key(&agent_id) {
            return Err(EngineError::AlreadySpawned(agent_id));
        }
        let inbox = self
            .bus
            .subscribe(initial_run.subscription_filter())
            .await
            .map_err(|error| EngineError::Bus(format!("agent subscribe failed: {error}")))?;
        let status_filter = crate::bus::SubscriptionFilter {
            session_ids: None,
            recipients: Some(vec![Recipient::Broadcast]),
            kinds: None,
        };
        let status_rx = self
            .bus
            .subscribe(status_filter)
            .await
            .map_err(|error| EngineError::Bus(format!("status subscribe failed: {error}")))?;

        let agent_id_clone = agent_id.clone();
        let expected_exit = Arc::new(AtomicBool::new(false));
        let task_expected_exit = expected_exit.clone();
        let exit_tx = self.exit_tx.clone();
        let task = tokio::spawn(async move {
            let _exit_signal = AgentExitSignal {
                agent_id: agent_id_clone.clone(),
                expected: task_expected_exit,
                sender: exit_tx,
            };
            run_revision_router(
                agent_id_clone.clone(),
                initial_revision,
                initial_run,
                provider,
                inbox,
            )
            .await;
            info!(agent_id = %agent_id_clone, "revisioned agent task exited");
        });
        let handle = AgentHandle {
            id: agent_id.clone(),
            spec,
            status: AgentStatus::Starting,
            status_rx,
            task: Some(task),
            expected_exit: expected_exit.clone(),
        };
        let ret = AgentHandle {
            id: handle.id.clone(),
            spec: handle.spec.clone(),
            status: handle.status,
            status_rx: mpsc::channel(1).1,
            task: None,
            expected_exit,
        };
        agents.insert(agent_id.clone(), handle);
        info!(agent_id = %agent_id, initial_revision, "revisioned agent spawned");
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
        let already_finished = {
            let mut agents = self.agents.write().await;
            let handle = agents
                .get_mut(agent_id)
                .ok_or_else(|| EngineError::NotFound(agent_id.clone()))?;
            handle.expected_exit.store(true, Ordering::SeqCst);
            handle.task.as_ref().is_some_and(JoinHandle::is_finished)
        };

        // Publish stop command via the bus
        if !already_finished {
            let stop_msg = BusMessage::system_stop(agent_id.clone());
            if let Err(error) = self.bus.publish(stop_msg).await {
                if let Some(handle) = self.agents.write().await.get_mut(agent_id) {
                    handle.expected_exit.store(false, Ordering::SeqCst);
                    if handle.task.as_ref().is_some_and(JoinHandle::is_finished) {
                        let _ = self.exit_tx.send(agent_id.clone());
                    }
                }
                return Err(EngineError::Bus(format!("stop publish failed: {error}")));
            }
        }

        // Wait for the agent to stop
        let stopped = if already_finished {
            true
        } else {
            let mut agents = self.agents.write().await;
            if let Some(handle) = agents.get_mut(agent_id) {
                handle.wait_for_status(AgentStatus::Stopped, 5000).await
            } else {
                return Err(EngineError::NotFound(agent_id.clone()));
            }
        };

        let mut handle = self
            .agents
            .write()
            .await
            .remove(agent_id)
            .ok_or_else(|| EngineError::NotFound(agent_id.clone()))?;
        if let Some(mut task) = handle.task.take() {
            if stopped {
                if tokio::time::timeout(tokio::time::Duration::from_secs(1), &mut task)
                    .await
                    .is_err()
                {
                    warn!(agent_id = %agent_id, "agent task did not exit after reporting stopped");
                    task.abort();
                    let _ = task.await;
                }
            } else {
                task.abort();
                let _ = task.await;
            }
        }

        if !stopped {
            return Err(EngineError::StopTimeout(agent_id.clone()));
        }

        info!(agent_id = %agent_id, "agent despawned");
        Ok(())
    }

    /// Wait until an Agent task exits without a matching despawn request.
    pub async fn wait_for_agent_exit(&self) -> Option<AgentId> {
        self.exits.lock().await.recv().await
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
                status_rx: mpsc::channel(1).1,
                task: None,
                expected_exit: handle.expected_exit.clone(),
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
            status_rx: mpsc::channel(1).1,
            task: None,
            expected_exit: h.expected_exit.clone(),
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
        self.attach_session(session_id.clone(), name, metadata, agent_ids)
            .await?;
        Ok(session_id)
    }

    /// Attach a session using its durable identity and notify its Agents.
    ///
    /// Persistent stores and external channels must retain the same ID across
    /// restarts; generating a replacement would orphan their history.
    pub async fn attach_session(
        &self,
        session_id: SessionId,
        name: impl Into<String>,
        metadata: SessionMetadata,
        agent_ids: &[AgentId],
    ) -> Result<(), EngineError> {
        if self.sessions.read().await.contains_key(&session_id) {
            return Err(EngineError::SessionAlreadyAttached(session_id));
        }
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
        Ok(())
    }

    /// Remove Runtime bookkeeping for a session during compensated creation.
    /// Agent-local authorization is revoked separately by the Runtime-owned
    /// session issuer path, so this method never publishes a public command.
    pub async fn detach_session(&self, session_id: &SessionId) -> bool {
        self.sessions.write().await.remove(session_id).is_some()
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

struct RevisionWorker {
    inbox: mpsc::Sender<BusMessage>,
    task: JoinHandle<()>,
}

async fn run_revision_router(
    agent_id: AgentId,
    initial_revision: u64,
    initial_run: AgentRun,
    provider: Arc<dyn RevisionedAgentRunProvider>,
    mut inbox: mpsc::Receiver<BusMessage>,
) {
    let mut workers = HashMap::new();
    workers.insert(initial_revision, spawn_revision_worker(initial_run));

    while let Some(message) = inbox.recv().await {
        if !matches!(&message.recipient, Recipient::Agent(id) if id == &agent_id) {
            continue;
        }
        if matches!(
            message.kind,
            MessageKind::Stream(_) | MessageKind::System(SystemMessage::StatusUpdate { .. })
        ) {
            continue;
        }
        if matches!(message.kind, MessageKind::System(SystemMessage::Stop)) {
            for worker in workers.values() {
                let _ = worker.inbox.send(message.clone()).await;
            }
            break;
        }
        let revision = match provider
            .revision_for_session(&agent_id, &message.session_id)
            .await
        {
            Ok(revision) => revision,
            Err(error) => {
                warn!(
                    %agent_id,
                    session_id = %message.session_id,
                    %error,
                    "failed to resolve Agent revision; dropping message"
                );
                continue;
            }
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = workers.entry(revision) {
            let run = match provider.run_for_revision(&agent_id, revision).await {
                Ok(run) if run.id() == &agent_id => run,
                Ok(run) => {
                    warn!(
                        %agent_id,
                        revision,
                        run_agent_id = %run.id(),
                        "revision provider returned a run for another Agent"
                    );
                    continue;
                }
                Err(error) => {
                    warn!(
                        %agent_id,
                        revision,
                        %error,
                        "failed to compose Agent revision; dropping message"
                    );
                    continue;
                }
            };
            entry.insert(spawn_revision_worker(run));
        }
        let delivered = if let Some(worker) = workers.get(&revision) {
            worker.inbox.send(message).await.is_ok()
        } else {
            false
        };
        if !delivered {
            warn!(%agent_id, revision, "Agent revision worker exited unexpectedly");
            workers.remove(&revision);
        }
    }

    // Closing an inbox is not sufficient: AgentRun waits for messages and
    // emits its terminal status only after receiving Stop.
    let stop = BusMessage::system_stop(agent_id);
    for worker in workers.values() {
        let _ = worker.inbox.send(stop.clone()).await;
    }
    for (_, worker) in workers {
        let _ = worker.task.await;
    }
}

fn spawn_revision_worker(run: AgentRun) -> RevisionWorker {
    let (inbox, receiver) = mpsc::channel(64);
    let task = tokio::spawn(run.run(receiver));
    RevisionWorker { inbox, task }
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

    /// The configured specification and run belong to different Agents.
    #[error("Agent run id {run} does not match specification id {spec}")]
    AgentRunMismatch { spec: AgentId, run: AgentId },

    /// Agent not found.
    #[error("agent not found: {0}")]
    NotFound(AgentId),

    /// Session not found.
    #[error("unknown session: {0}")]
    UnknownSession(SessionId),

    /// A durable session was restored more than once.
    #[error("session already attached: {0}")]
    SessionAlreadyAttached(SessionId),

    /// `AgentRun` build failed.
    #[error("build error: {0}")]
    Build(#[from] crate::run::AgentRunError),

    /// Bus operation failed.
    #[error("bus error: {0}")]
    Bus(String),

    /// An Agent did not reach its terminal state before the shutdown deadline.
    #[error("agent did not stop within timeout: {0}")]
    StopTimeout(AgentId),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../tests/unit/engine.rs"]
mod tests;
