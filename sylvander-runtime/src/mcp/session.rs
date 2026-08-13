//! Authenticated Session ownership for MCP runtime state.
//!
//! Attaching records unresolved server declarations and exact identity before
//! any secret or process exists. Later connection/environment modules consume
//! this state only after a workspace and sandbox policy have been admitted.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use thiserror::Error;

use crate::agent_definition::{McpServerConfig, SessionId};
use sylvander_api::AgentId;

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
#[derive(Clone, Default)]
pub(crate) struct SessionMcpRuntimeService {
    sessions: Arc<RwLock<HashMap<SessionId, Arc<SessionMcpRuntime>>>>,
}

struct SessionMcpRuntime {
    binding: SessionMcpBinding,
    servers: Arc<[McpServerConfig]>,
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
}

impl SessionMcpRuntimeService {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Attach declarations to one authenticated Session without resolving
    /// secrets or starting a process.
    pub(crate) fn attach(
        &self,
        binding: SessionMcpBinding,
        servers: Vec<McpServerConfig>,
    ) -> Result<(), SessionMcpError> {
        validate_binding(&binding)?;
        validate_servers(&servers)?;
        let session_id = binding.session_id.clone();
        let runtime = Arc::new(SessionMcpRuntime {
            binding,
            servers: servers.into(),
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
    pub(crate) fn detach(&self, session_id: &SessionId) {
        let runtime = self
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
        if let Some(runtime) = runtime {
            runtime.drain();
        }
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
    fn drain(&self) {
        tracing::debug!(
            agent_id = %self.binding.agent_id,
            session_id = %self.binding.session_id,
            server_count = self.servers.len(),
            "draining Session-owned MCP runtime"
        );
        self.state.store(STATE_STOPPED, Ordering::Release);
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
