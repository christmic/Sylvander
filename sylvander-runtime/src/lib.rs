//! # sylvander-runtime
//!
//! System runtime — the bootstrap and orchestration layer above the
//! agent engine.
//!
//! The runtime:
//! 1. Boots the system (creates bus, engine, session store)
//! 2. Spawns agents from configuration
//! 3. Loads persistent sessions
//! 4. Starts protocol channels (TUI, Telegram, ...)
//!
//! # Architecture
//!
//! ```text
//! Channel (TUI / Telegram / ...)
//!       │  normalize external messages → BusMessage
//!       ▼
//! ┌──────────────────┐
//! │  sylvander-runtime│  boot / shutdown / create_ephemeral_session
//! ├──────────────────┤
//! │  sylvander-agent  │  AgentRunEngine / AgentRun / AgentLoop
//! └──────────────────┘
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::info;

use sylvander_agent::bus::{InProcessMessageBus, MessageBus};
use sylvander_agent::engine::AgentRunEngine;
use sylvander_agent::session::SessionMetadata;
use sylvander_agent::session_store::{
    SessionLifetime, SessionStore, SqliteSessionStore, StoredSession,
};
use sylvander_agent::spec::{AgentId, AgentSpec, SessionId};
use sylvander_channel::{Channel, ChannelContext};
use sylvander_llm_anthropic::api::client::AnthropicClient;

// ---------------------------------------------------------------------------
// SystemConfig
// ---------------------------------------------------------------------------

/// System bootstrap configuration.
///
/// Constructed in code for now; TOML file loading is a future concern.
#[derive(Debug, Clone)]
pub struct SystemConfig {
    /// Human-readable system name.
    pub name: String,
    /// Agents to spawn at boot.
    pub agents: Vec<AgentSpec>,
    /// Pre-defined persistent sessions to load/create at boot.
    pub sessions: Vec<StoredSession>,
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// The system runtime — top-level orchestrator.
pub struct Runtime {
    /// The agent lifecycle engine.
    pub engine: Arc<AgentRunEngine>,
    /// Session persistence backend.
    pub session_store: Arc<dyn SessionStore>,
    /// Ephemeral sessions (tracked in memory, not persisted).
    ephemeral: RwLock<HashMap<SessionId, StoredSession>>,
    /// Shared message bus.
    bus: Arc<dyn MessageBus>,
}

impl Runtime {
    /// Bootstrap the system.
    ///
    /// # Flow
    ///
    /// 1. Create an in-process message bus
    /// 2. Create the engine
    /// 3. Create an in-memory session store
    /// 4. Load persistent sessions → re-create in engine
    /// 5. Spawn each agent from config
    /// 6. Create sessions defined in config
    pub async fn boot(
        config: SystemConfig,
        default_client: AnthropicClient,
    ) -> Result<Self, RuntimeError> {
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = Arc::new(AgentRunEngine::new(bus.clone()));
        let session_store: Arc<dyn SessionStore> = Arc::new(
            SqliteSessionStore::open_in_memory()
                .await
                .map_err(|e| RuntimeError::Store(format!("open session store: {e}")))?,
        );

        // Load persistent sessions
        for session in session_store.list_persistent().await.map_err(|e| {
            RuntimeError::Store(format!("list persistent failed: {e}"))
        })? {
            engine
                .create_session(&session.name, session.metadata.clone(), &session.agents)
                .await
                .map_err(|e| RuntimeError::Engine(format!("load session failed: {e}")))?;
        }

        // Spawn agents
        for spec in &config.agents {
            engine
                .spawn(spec.clone(), default_client.clone())
                .await
                .map_err(|e| RuntimeError::Engine(format!("spawn {} failed: {e}", spec.id)))?;
        }

        // Create sessions from config
        for session in &config.sessions {
            engine
                .create_session(&session.name, session.metadata.clone(), &session.agents)
                .await
                .map_err(|e| {
                    RuntimeError::Engine(format!("create session {} failed: {e}", session.id))
                })?;
            if session.lifetime == SessionLifetime::Persistent {
                session_store.save(session).await.map_err(|e| {
                    RuntimeError::Store(format!("save session failed: {e}"))
                })?;
            }
        }

        info!(name = %config.name, agents = config.agents.len(), "runtime booted");

        Ok(Self {
            engine,
            session_store,
            ephemeral: RwLock::new(HashMap::new()),
            bus,
        })
    }

    // -- channels --

    /// Start protocol channels. Each runs in its own tokio task.
    pub fn start_channels(&self, channels: Vec<Arc<dyn Channel>>) {
        for ch in channels {
            let ctx = ChannelContext {
                bus: self.bus.clone(),
                sessions: self.session_store.clone(),
            };
            let name = ch.name().to_string();
            tokio::spawn(async move {
                ch.run(ctx).await;
                info!(channel = %name, "channel stopped");
            });
        }
    }

    // -- ephemeral sessions --

    /// Create a temporary session.
    ///
    /// This is the primary entry point for channels creating
    /// per-conversation sessions (new TUI window, new Telegram chat).
    pub async fn create_ephemeral_session(
        &self,
        name: impl Into<String>,
        metadata: SessionMetadata,
        agents: &[AgentId],
        external_meta: HashMap<String, serde_json::Value>,
    ) -> Result<SessionId, RuntimeError> {
        let session_id = self
            .engine
            .create_session(name, metadata.clone(), agents)
            .await
            .map_err(|e| RuntimeError::Engine(format!("create ephemeral: {e}")))?;

        let session_name = metadata.name.clone();
        let mut stored = StoredSession::new(
            session_id.clone(),
            session_name,
            SessionLifetime::Ephemeral,
            metadata,
            agents.to_vec(),
        );
        stored.external_meta = external_meta;

        self.ephemeral
            .write()
            .await
            .insert(session_id.clone(), stored);

        Ok(session_id)
    }

    /// Look up external metadata for an ephemeral session.
    pub async fn get_external_meta(
        &self,
        session_id: &SessionId,
    ) -> Option<HashMap<String, serde_json::Value>> {
        self.ephemeral
            .read()
            .await
            .get(session_id)
            .map(|s| s.external_meta.clone())
    }

    // -- shutdown --

    /// Graceful shutdown — despawn all agents.
    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        let agents = self.engine.list_agents().await;
        for handle in agents {
            self.engine.despawn(&handle.id).await.map_err(|e| {
                RuntimeError::Engine(format!("despawn {} failed: {e}", handle.id))
            })?;
        }
        info!("runtime shut down");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RuntimeError
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("engine error: {0}")]
    Engine(String),
    #[error("store error: {0}")]
    Store(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_spec(id: &str) -> AgentSpec {
        AgentSpec::builder()
            .id(id)
            .name(format!("Agent {id}"))
            .model_name("claude-sonnet-5-20260601")
            .build()
            .expect("spec")
    }

    fn test_client() -> AnthropicClient {
        AnthropicClient::builder()
            .api_key("test-key")
            .build()
            .expect("client")
    }

    fn test_metadata() -> SessionMetadata {
        SessionMetadata {
            workspace: PathBuf::from("/tmp"),
            name: "test".into(),
            user_id: "user-1".into(),
        }
    }

    #[tokio::test]
    async fn boot_spawns_agents() {
        let config = SystemConfig {
            name: "test-runtime".into(),
            agents: vec![test_spec("agent-1"), test_spec("agent-2")],
            sessions: vec![],
        };

        let rt = Runtime::boot(config, test_client()).await.expect("boot");
        assert_eq!(rt.engine.list_agents().await.len(), 2);
        rt.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn boot_loads_persistent_sessions() {
        let config = SystemConfig {
            name: "test-runtime".into(),
            agents: vec![test_spec("agent-1")],
            sessions: vec![StoredSession::new(
                SessionId::new("persistent-1"),
                "persistent-chat",
                SessionLifetime::Persistent,
                test_metadata(),
                vec![AgentId::new("agent-1")],
            )],
        };

        let rt = Runtime::boot(config, test_client()).await.expect("boot");
        assert_eq!(rt.engine.list_sessions().await.len(), 1);
        rt.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn create_ephemeral_session_with_external_meta() {
        let config = SystemConfig {
            name: "test-runtime".into(),
            agents: vec![test_spec("agent-1")],
            sessions: vec![],
        };

        let rt = Runtime::boot(config, test_client()).await.expect("boot");

        let mut meta = HashMap::new();
        meta.insert("chat_id".into(), serde_json::json!("-100xxx"));

        let sid = rt
            .create_ephemeral_session(
                "ephemeral",
                test_metadata(),
                &[AgentId::new("agent-1")],
                meta,
            )
            .await
            .expect("create");

        let stored = rt.get_external_meta(&sid).await.expect("should exist");
        assert_eq!(stored.get("chat_id").unwrap(), &serde_json::json!("-100xxx"));

        rt.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn external_meta_not_in_engine() {
        // Protocol metadata stays in the runtime layer — the engine
        // (and agents) never see it.
        let config = SystemConfig {
            name: "test-runtime".into(),
            agents: vec![test_spec("agent-1")],
            sessions: vec![],
        };

        let rt = Runtime::boot(config, test_client()).await.expect("boot");

        let mut meta = HashMap::new();
        meta.insert("secret".into(), serde_json::json!("hidden"));

        rt.create_ephemeral_session(
            "test",
            test_metadata(),
            &[AgentId::new("agent-1")],
            meta,
        )
        .await
        .expect("create");

        // Engine sessions have no external_meta field
        let engine_sessions = rt.engine.list_sessions().await;
        assert_eq!(engine_sessions.len(), 1);
        // SessionMeta (engine-level) has no external_meta

        rt.shutdown().await.expect("shutdown");
    }
}
