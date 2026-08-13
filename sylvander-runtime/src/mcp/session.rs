//! Authenticated Session ownership for MCP runtime state.
//!
//! Attaching records unresolved server declarations and exact identity before
//! any secret or process exists. Later connection/environment modules consume
//! this state only after a workspace and sandbox policy have been admitted.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use thiserror::Error;

use crate::agent_definition::{McpServerConfig, McpWorkspaceAccess, SessionId};
use crate::config::SecretRef;
use crate::credential_registry::CredentialSecretResolver;
use crate::execution::{
    PersistentFilesystemAuthority, PersistentNetworkAuthority, PersistentProcessAuthority,
    PersistentProcessOwner, PersistentResourceLimits, RuntimeExecutionService,
};
use crate::mcp::SECRET_REFERENCE_PREFIX;
use crate::mcp_stdio::{McpResultArtifactSink, McpStdioClient};
use sylvander_agent::tool::ToolRegistry;
use sylvander_api::{AgentId, AgentSecretReference};

const STATE_CONFIGURED: u8 = 1;
const STATE_STOPPED: u8 = 2;

/// Exact non-secret identity that owns one Session MCP runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionMcpBinding {
    pub(crate) user_id: String,
    pub(crate) agent_id: AgentId,
    pub(crate) session_id: SessionId,
    pub(crate) policy_revision: u64,
}

/// Cloneable Runtime service shared by one configured Agent revision.
///
/// Clones share the same map; declarations for different Sessions never share
/// a runtime value or mutable lifecycle state.
#[derive(Clone)]
pub(crate) struct SessionMcpRuntimeService {
    sessions: Arc<RwLock<HashMap<SessionId, Arc<SessionMcpRuntime>>>>,
    execution: RuntimeExecutionService,
    secrets: Option<Arc<dyn CredentialSecretResolver>>,
    result_artifacts: Option<Arc<dyn McpResultArtifactSink>>,
}

struct SessionMcpRuntime {
    binding: SessionMcpBinding,
    servers: Arc<[McpServerConfig]>,
    clients: Arc<[McpStdioClient]>,
    tools: ToolRegistry,
    state: AtomicU8,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum SessionMcpError {
    #[error("invalid MCP Session binding")]
    InvalidBinding,
    #[error("duplicate MCP server name `{0}`")]
    DuplicateServer(String),
    #[error("MCP runtime already exists for Session `{0}`")]
    DuplicateSession(String),
    #[error("MCP server `{server}` references unknown execution environment `{environment}`")]
    UnknownEnvironment { server: String, environment: String },
    #[error("MCP server `{0}` execution environment is not an enforcing sandbox")]
    UnconfinedEnvironment(String),
    #[error("MCP server `{0}` secret reference could not be resolved")]
    Secret(String),
    #[error("MCP server `{server}` connection failed: {message}")]
    Connection { server: String, message: String },
}

impl SessionMcpRuntimeService {
    pub(crate) fn new(
        execution: RuntimeExecutionService,
        secrets: Option<Arc<dyn CredentialSecretResolver>>,
        result_artifacts: Option<Arc<dyn McpResultArtifactSink>>,
    ) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            execution,
            secrets,
            result_artifacts,
        }
    }

    /// Resolve secrets and start each server only after the authenticated
    /// Session workspace and enforcing environment have been admitted.
    pub(crate) async fn attach(
        &self,
        binding: SessionMcpBinding,
        servers: Vec<McpServerConfig>,
        workspace_root: PathBuf,
    ) -> Result<(), SessionMcpError> {
        validate_binding(&binding)?;
        validate_servers(&servers)?;
        let mut clients = Vec::with_capacity(servers.len());
        let mut tools = ToolRegistry::new();
        for declaration in &servers {
            let environment = self
                .execution
                .resolve_persistent(&declaration.execution_environment)
                .cloned()
                .ok_or_else(|| SessionMcpError::UnknownEnvironment {
                    server: declaration.name.clone(),
                    environment: declaration.execution_environment.clone(),
                })?;
            if environment.name() != declaration.execution_environment
                || !environment.isolation().enforces_required_boundary()
            {
                return Err(SessionMcpError::UnconfinedEnvironment(
                    declaration.name.clone(),
                ));
            }
            let server = self.resolve_server_environment(declaration)?;
            let filesystem = match declaration.workspace_access {
                McpWorkspaceAccess::Read => PersistentFilesystemAuthority::WorkspaceRead,
                McpWorkspaceAccess::Write => PersistentFilesystemAuthority::WorkspaceWrite,
            };
            let authority = PersistentProcessAuthority {
                owner: PersistentProcessOwner {
                    principal_id: binding.user_id.clone(),
                    workload_id: format!("{}:mcp:{}", binding.agent_id, declaration.name),
                    session_id: binding.session_id.0.clone(),
                    policy_revision: binding.policy_revision,
                },
                workspace_root: workspace_root.clone(),
                filesystem,
                network: PersistentNetworkAuthority::Denied,
                resources: PersistentResourceLimits::default(),
                startup_timeout: Duration::from_secs(30),
                drain_timeout: Duration::from_secs(5),
            };
            let client = match &self.result_artifacts {
                Some(sink) => {
                    McpStdioClient::connect_with_result_artifact_sink(
                        &server,
                        authority.startup_timeout,
                        sink.clone(),
                        environment,
                        authority,
                    )
                    .await
                }
                None => {
                    McpStdioClient::connect_in(
                        &server,
                        authority.startup_timeout,
                        environment,
                        authority,
                    )
                    .await
                }
            }
            .map_err(|error| SessionMcpError::Connection {
                server: declaration.name.clone(),
                message: error.to_string(),
            })?;
            client
                .list_tools()
                .await
                .map_err(|error| SessionMcpError::Connection {
                    server: declaration.name.clone(),
                    message: error.to_string(),
                })?;
            tools = tools.register_dynamic_source(client.clone());
            clients.push(client);
        }
        let session_id = binding.session_id.clone();
        let runtime = Arc::new(SessionMcpRuntime {
            binding,
            servers: servers.into(),
            clients: clients.into(),
            tools,
            state: AtomicU8::new(STATE_CONFIGURED),
        });
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if sessions.contains_key(&session_id) {
            return Err(SessionMcpError::DuplicateSession(session_id.0));
        }
        sessions.insert(session_id, runtime);
        Ok(())
    }

    /// Remove Session ownership before its process resources are drained.
    /// New turns cannot obtain the detached runtime.
    pub(crate) async fn detach(&self, session_id: &SessionId) {
        let runtime = self
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
        if let Some(runtime) = runtime {
            runtime.drain().await;
        }
    }

    /// Return the neutral catalog for one ready Session. The caller freezes
    /// it together with the Agent revision before publishing the turn.
    pub(crate) fn tool_registry(&self, session_id: &SessionId) -> Option<ToolRegistry> {
        self.sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .filter(|runtime| runtime.state.load(Ordering::Acquire) == STATE_CONFIGURED)
            .map(|runtime| runtime.tools.clone())
    }

    fn resolve_server_environment(
        &self,
        declaration: &McpServerConfig,
    ) -> Result<McpServerConfig, SessionMcpError> {
        let mut server = declaration.clone();
        let mut environment = HashMap::with_capacity(server.envs.len());
        for (name, encoded) in &server.envs {
            let resolver = self
                .secrets
                .as_ref()
                .ok_or_else(|| SessionMcpError::Secret(server.name.clone()))?;
            let reference = decode_secret_reference(encoded)
                .ok_or_else(|| SessionMcpError::Secret(server.name.clone()))?;
            let value = resolver
                .resolve_credential(&reference)
                .map_err(|()| SessionMcpError::Secret(server.name.clone()))?;
            let value = value
                .as_str()
                .map_err(|_| SessionMcpError::Secret(server.name.clone()))?;
            environment.insert(name.clone(), value.to_owned());
        }
        server.envs = environment;
        Ok(server)
    }

    #[cfg(test)]
    pub(crate) fn inspect(&self, session_id: &SessionId) -> Option<SessionMcpInspection> {
        self.sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .map(|runtime| SessionMcpInspection {
                binding: runtime.binding.clone(),
                server_count: runtime.servers.len(),
                configured: runtime.state.load(Ordering::Acquire) == STATE_CONFIGURED,
            })
    }
}

impl SessionMcpRuntime {
    async fn drain(&self) {
        tracing::debug!(
            agent_id = %self.binding.agent_id,
            session_id = %self.binding.session_id,
            server_count = self.servers.len(),
            "draining Session-owned MCP runtime"
        );
        self.state.store(STATE_STOPPED, Ordering::Release);
        for client in self.clients.iter() {
            if let Err(error) = client.shutdown().await {
                tracing::warn!(
                    agent_id = %self.binding.agent_id,
                    session_id = %self.binding.session_id,
                    error = %error,
                    "failed to stop Session-owned MCP server"
                );
            }
        }
    }
}

fn decode_secret_reference(encoded: &str) -> Option<SecretRef> {
    let encoded = encoded.strip_prefix(SECRET_REFERENCE_PREFIX)?;
    match serde_json::from_str::<AgentSecretReference>(encoded).ok()? {
        AgentSecretReference::Environment { name } => Some(SecretRef::Env { name }),
        AgentSecretReference::File { path } => Some(SecretRef::File { path: path.into() }),
    }
}

fn validate_binding(binding: &SessionMcpBinding) -> Result<(), SessionMcpError> {
    if binding.user_id.trim().is_empty()
        || binding.agent_id.0.trim().is_empty()
        || binding.session_id.0.trim().is_empty()
        || binding.policy_revision == 0
    {
        return Err(SessionMcpError::InvalidBinding);
    }
    Ok(())
}

fn validate_servers(servers: &[McpServerConfig]) -> Result<(), SessionMcpError> {
    let mut names = HashSet::new();
    for server in servers {
        if server.name.trim().is_empty() || !names.insert(server.name.clone()) {
            return Err(SessionMcpError::DuplicateServer(server.name.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) struct SessionMcpInspection {
    pub(crate) binding: SessionMcpBinding,
    pub(crate) server_count: usize,
    pub(crate) configured: bool,
}

#[cfg(test)]
#[path = "../../tests/unit/mcp_session.rs"]
mod tests;
