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

pub mod composition;
pub mod config;
pub mod evidence;

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use sylvander_agent::bus::{InProcessMessageBus, MessageBus};
use sylvander_agent::engine::AgentRunEngine;
use sylvander_agent::session::SessionMetadata;
use sylvander_agent::session_store::{
    SessionLifetime, SessionStore, SqliteSessionStore, StoredSession,
};
use sylvander_agent::spec::{AgentId, AgentSpec, SessionId};
use sylvander_channel::{Channel, ChannelContext, ChannelReadiness};
use sylvander_llm_anthropic::api::client::AnthropicClient;

use crate::composition::{ConfiguredAgent, build_agents};
use crate::config::{ServerConfig, SystemSecretResolver};
use crate::evidence::{EvidenceRecorder, EvidenceStore};

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
    /// Fully configured runs retained for protocol control operations.
    configured_agents: HashMap<AgentId, ConfiguredAgent>,
    evidence: Option<EvidenceRecorder>,
    channels: tokio::sync::Mutex<Vec<ChannelTask>>,
}

struct ChannelTask {
    name: String,
    task: JoinHandle<()>,
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

        // Spawn agents
        for spec in &config.agents {
            engine
                .spawn(spec.clone(), default_client.clone())
                .await
                .map_err(|e| RuntimeError::Engine(format!("spawn {} failed: {e}", spec.id)))?;
        }

        // Restore durable identities only after Agents subscribe to the bus.
        for session in session_store
            .list_persistent()
            .await
            .map_err(|e| RuntimeError::Store(format!("list persistent failed: {e}")))?
        {
            engine
                .attach_session(
                    session.id.clone(),
                    &session.name,
                    session.metadata.clone(),
                    &session.agents,
                )
                .await
                .map_err(|e| RuntimeError::Engine(format!("load session failed: {e}")))?;
        }

        // Create sessions from config
        for session in &config.sessions {
            engine
                .attach_session(
                    session.id.clone(),
                    &session.name,
                    session.metadata.clone(),
                    &session.agents,
                )
                .await
                .map_err(|e| {
                    RuntimeError::Engine(format!("create session {} failed: {e}", session.id))
                })?;
            if session.lifetime == SessionLifetime::Persistent {
                session_store
                    .save(session)
                    .await
                    .map_err(|e| RuntimeError::Store(format!("save session failed: {e}")))?;
            }
        }

        info!(name = %config.name, agents = config.agents.len(), "runtime booted");

        Ok(Self {
            engine,
            session_store,
            ephemeral: RwLock::new(HashMap::new()),
            bus,
            configured_agents: HashMap::new(),
            evidence: None,
            channels: tokio::sync::Mutex::new(Vec::new()),
        })
    }

    /// Bootstrap the production runtime from validated server configuration.
    pub async fn boot_config(config: ServerConfig) -> Result<Self, RuntimeError> {
        config
            .validate()
            .map_err(|error| RuntimeError::Config(error.to_string()))?;
        let config = with_resolved_paths(config)?;
        let session_db = config
            .server
            .session_db
            .as_ref()
            .expect("resolved session database");
        if let Some(parent) = session_db.parent() {
            std::fs::create_dir_all(parent).map_err(|error| RuntimeError::Io {
                operation: "create session database directory",
                path: parent.display().to_string(),
                message: error.to_string(),
            })?;
        }

        let session_store: Arc<dyn SessionStore> = Arc::new(
            SqliteSessionStore::open(session_db)
                .await
                .map_err(|error| RuntimeError::Store(error.to_string()))?,
        );
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = Arc::new(AgentRunEngine::new(bus.clone()));
        let evidence = if config.server.evidence.enabled {
            let path = config
                .server
                .evidence
                .path
                .as_ref()
                .expect("resolved evidence path");
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| RuntimeError::Io {
                    operation: "create evidence directory",
                    path: parent.display().to_string(),
                    message: error.to_string(),
                })?;
            }
            let store = EvidenceStore::open(path)
                .await
                .map_err(|error| RuntimeError::Evidence(error.to_string()))?;
            Some(
                EvidenceRecorder::start(
                    bus.clone(),
                    store,
                    config.server.name.clone(),
                    config.server.evidence.content,
                    config.server.evidence.retention_days,
                )
                .await
                .map_err(|error| RuntimeError::Evidence(error.to_string()))?,
            )
        } else {
            None
        };
        let agents = build_agents(
            &config,
            bus.clone(),
            session_store.clone(),
            &SystemSecretResolver,
        )
        .map_err(|error| RuntimeError::Composition(error.to_string()))?;
        let mut configured_agents = HashMap::new();
        for agent in agents {
            engine
                .spawn_run(agent.spec.clone(), agent.run.clone())
                .await
                .map_err(|error| RuntimeError::Engine(error.to_string()))?;
            configured_agents.insert(agent.spec.id.clone(), agent);
        }

        for session in session_store
            .list_persistent()
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
        {
            engine
                .attach_session(
                    session.id.clone(),
                    &session.name,
                    session.metadata,
                    &session.agents,
                )
                .await
                .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        }

        info!(
            name = %config.server.name,
            agents = configured_agents.len(),
            session_db = %session_db.display(),
            "configured runtime booted"
        );
        Ok(Self {
            engine,
            session_store,
            ephemeral: RwLock::new(HashMap::new()),
            bus,
            configured_agents,
            evidence,
            channels: tokio::sync::Mutex::new(Vec::new()),
        })
    }

    /// Return protocol metadata and control for one configured Agent.
    #[must_use]
    pub fn configured_agent(&self, id: &AgentId) -> Option<&ConfiguredAgent> {
        self.configured_agents.get(id)
    }

    /// Return the shared message bus used by protocol adapters.
    #[must_use]
    pub fn bus(&self) -> Arc<dyn MessageBus> {
        self.bus.clone()
    }

    /// Return the durable evidence store when collection is enabled.
    #[must_use]
    pub fn evidence_store(&self) -> Option<EvidenceStore> {
        self.evidence.as_ref().map(EvidenceRecorder::store)
    }

    // -- channels --

    /// Start protocol channels. Each runs in its own tokio task.
    pub async fn start_channels(
        &self,
        channels: Vec<Arc<dyn Channel>>,
    ) -> Result<(), RuntimeError> {
        let mut tasks = self.channels.lock().await;
        if !tasks.is_empty() {
            return Err(RuntimeError::Channel(
                "channels have already been started".into(),
            ));
        }
        for ch in channels {
            let readiness = ChannelReadiness::new();
            let ctx = ChannelContext {
                bus: self.bus.clone(),
                sessions: self.session_store.clone(),
                readiness: Some(readiness.clone()),
            };
            let name = ch.name().to_string();
            let task_name = name.clone();
            let mut task = tokio::spawn(async move {
                ch.run(ctx).await;
                warn!(channel = %task_name, "channel task exited");
            });
            tokio::select! {
                result = &mut task => {
                    return Err(RuntimeError::Channel(match result {
                        Ok(()) => format!("channel {name} exited before becoming ready"),
                        Err(error) => format!("channel {name} failed during startup: {error}"),
                    }));
                }
                result = tokio::time::timeout(
                    tokio::time::Duration::from_secs(5),
                    readiness.wait(),
                ) => {
                    if result.is_err() {
                        task.abort();
                        let _ = task.await;
                        return Err(RuntimeError::Channel(format!(
                            "channel {name} did not become ready within 5 seconds"
                        )));
                    }
                }
            }
            info!(channel = %name, "channel ready");
            tasks.push(ChannelTask { name, task });
        }
        Ok(())
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
        // Stop accepting external work before stopping the Agents that serve it.
        let channel_tasks = {
            let mut tasks = self.channels.lock().await;
            tasks.drain(..).collect::<Vec<_>>()
        };
        for channel in &channel_tasks {
            channel.task.abort();
        }
        for channel in channel_tasks {
            match channel.task.await {
                Ok(()) => info!(channel = %channel.name, "channel stopped"),
                Err(error) if error.is_cancelled() => {
                    info!(channel = %channel.name, "channel cancelled during shutdown");
                }
                Err(error) => {
                    return Err(RuntimeError::Channel(format!(
                        "channel {} task failed: {error}",
                        channel.name
                    )));
                }
            }
        }
        let agents = self.engine.list_agents().await;
        for handle in agents {
            self.engine
                .despawn(&handle.id)
                .await
                .map_err(|e| RuntimeError::Engine(format!("despawn {} failed: {e}", handle.id)))?;
        }
        if let Some(evidence) = &self.evidence {
            evidence
                .shutdown()
                .await
                .map_err(|error| RuntimeError::Evidence(error.to_string()))?;
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
    #[error("configuration error: {0}")]
    Config(String),
    #[error("composition error: {0}")]
    Composition(String),
    #[error("evidence error: {0}")]
    Evidence(String),
    #[error("channel error: {0}")]
    Channel(String),
    #[error("{operation} at {path}: {message}")]
    Io {
        operation: &'static str,
        path: String,
        message: String,
    },
}

fn with_resolved_paths(mut config: ServerConfig) -> Result<ServerConfig, RuntimeError> {
    let data_dir = config.server.data_dir.clone().unwrap_or_else(|| {
        std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| std::path::PathBuf::from(home).join(".local/share"))
            })
            .unwrap_or_else(|| std::path::PathBuf::from(".local/share"))
            .join("sylvander")
    });
    std::fs::create_dir_all(&data_dir).map_err(|error| RuntimeError::Io {
        operation: "create data directory",
        path: data_dir.display().to_string(),
        message: error.to_string(),
    })?;
    config.server.data_dir = Some(data_dir.clone());
    config
        .server
        .session_db
        .get_or_insert_with(|| data_dir.join("sessions.db"));
    config
        .server
        .workspace_journal
        .get_or_insert_with(|| data_dir.join("workspace-journal"));
    if config.server.evidence.enabled {
        config
            .server
            .evidence
            .path
            .get_or_insert_with(|| data_dir.join("evidence.db"));
    }
    Ok(config)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    struct BlockingChannel {
        started: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    }

    struct ExitingChannel;

    #[async_trait::async_trait]
    impl Channel for ExitingChannel {
        fn name(&self) -> &'static str {
            "exiting-test"
        }

        async fn run(self: Arc<Self>, _ctx: ChannelContext) {}
    }

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl Channel for BlockingChannel {
        fn name(&self) -> &'static str {
            "blocking-test"
        }

        async fn run(self: Arc<Self>, ctx: ChannelContext) {
            let _drop_signal = DropSignal(self.dropped.clone());
            ctx.mark_ready();
            self.started.notify_one();
            std::future::pending::<()>().await;
        }
    }

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
    async fn shutdown_cancels_owned_channel_tasks_before_returning() {
        let runtime = Runtime::boot(
            SystemConfig {
                name: "test-runtime".into(),
                agents: Vec::new(),
                sessions: Vec::new(),
            },
            test_client(),
        )
        .await
        .unwrap();
        let started = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        runtime
            .start_channels(vec![Arc::new(BlockingChannel {
                started: started.clone(),
                dropped: dropped.clone(),
            })])
            .await
            .unwrap();
        started.notified().await;

        runtime.shutdown().await.unwrap();
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn channel_exit_before_readiness_fails_startup() {
        let runtime = Runtime::boot(
            SystemConfig {
                name: "test-runtime".into(),
                agents: Vec::new(),
                sessions: Vec::new(),
            },
            test_client(),
        )
        .await
        .unwrap();

        let error = runtime
            .start_channels(vec![Arc::new(ExitingChannel)])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("before becoming ready"));
        runtime.shutdown().await.unwrap();
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
        assert!(
            rt.engine
                .get_session(&SessionId::new("persistent-1"))
                .await
                .is_some()
        );
        rt.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn configured_boot_restores_database_session_after_agent_spawn() {
        let directory = tempfile::TempDir::new().unwrap();
        let database = directory.path().join("sessions.db");
        let secret = directory.path().join("provider.key");
        std::fs::write(&secret, "test-secret").unwrap();
        let store = SqliteSessionStore::open(&database).await.unwrap();
        store
            .save(&StoredSession::new(
                SessionId::new("restored-session"),
                "restored",
                SessionLifetime::Persistent,
                test_metadata(),
                vec![AgentId::new("assistant")],
            ))
            .await
            .unwrap();
        drop(store);

        let input = format!(
            r#"
schema_version = 1

[server]
data_dir = "{}"
session_db = "{}"

[[model_providers]]
id = "primary"
base_url = "https://models.example.test"

[model_providers.api_key]
source = "file"
path = "{}"

[[model_providers.models]]
id = "model-a"
capabilities = ["tool_use"]

[[agents]]
allow_session_prompt = false

[agents.spec]
id = "assistant"
name = "Sylvander"

[agents.spec.model]
provider = "primary"
model_name = "model-a"
"#,
            directory.path().display(),
            database.display(),
            secret.display()
        );
        let config = ServerConfig::from_toml(&input).unwrap();
        let runtime = Runtime::boot_config(config).await.unwrap();

        assert!(
            runtime
                .engine
                .get_session(&SessionId::new("restored-session"))
                .await
                .is_some()
        );
        assert!(
            runtime
                .configured_agent(&AgentId::new("assistant"))
                .is_some()
        );
        let evidence = runtime
            .evidence_store()
            .expect("evidence enabled by default");
        runtime.shutdown().await.unwrap();
        let counts = evidence.counts().await.unwrap();
        assert_eq!(counts.runs, 1);
        assert!(counts.events >= 1, "Agent lifecycle must reach evidence");
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
        assert_eq!(
            stored.get("chat_id").unwrap(),
            &serde_json::json!("-100xxx")
        );

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

        rt.create_ephemeral_session("test", test_metadata(), &[AgentId::new("agent-1")], meta)
            .await
            .expect("create");

        // Engine sessions have no external_meta field
        let engine_sessions = rt.engine.list_sessions().await;
        assert_eq!(engine_sessions.len(), 1);
        // SessionMeta (engine-level) has no external_meta

        rt.shutdown().await.expect("shutdown");
    }
}
