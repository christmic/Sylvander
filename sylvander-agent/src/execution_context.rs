//! Trusted, non-wire execution authority for one Agent turn.
//!
//! Runtime constructs these values after authentication, Session admission,
//! workspace selection, and policy resolution. The absence of Serde derives is
//! intentional: API input and model-generated JSON must never become execution
//! authority without crossing Runtime's validation boundary.

use std::collections::BTreeSet;
use std::time::Duration;

/// Runtime-derived actor identity used for authorization and audit.
///
/// Values are deliberately Agent-owned strings rather than API identifier
/// types. Runtime validates and maps external identities before construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionActor {
    /// Stable Runtime-validated user identity.
    pub user_id: String,
    /// Exact Runtime-pinned Agent identity.
    pub agent_id: String,
    /// Product Session identity used only for authorization and correlation.
    pub session_id: String,
}

impl ExecutionActor {
    /// Construct an actor from already validated Runtime identities.
    #[must_use]
    pub fn new(
        user_id: impl Into<String>,
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            agent_id: agent_id.into(),
            session_id: session_id.into(),
        }
    }
}

/// Workspace selected and authorized by Runtime before Agent execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionWorkspace {
    /// Logical workspace binding selected by Runtime.
    pub workspace_id: String,
    /// Logical execution target resolved by Runtime's adapter registry.
    pub target_id: String,
    /// Whether every mutating workspace operation must be rejected.
    pub read_only: bool,
}

/// Authority categories understood by the Agent execution kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExecutionCapability {
    /// Read data through the selected workspace port.
    WorkspaceRead,
    /// Mutate data through the selected workspace port.
    WorkspaceWrite,
    /// Launch a process only through an enforcing execution environment.
    Process,
    /// Request network authority subject to the prepared tool policy.
    Network,
    /// Perform bounded repository operations.
    Git,
    /// Retrieve memory context selected for the actor.
    MemoryRead,
    /// Submit a candidate to Runtime's governed memory flow.
    MemoryCandidate,
}

/// Complete trusted authority snapshot for one Agent execution.
///
/// This type intentionally has no Serde implementation. A client or model
/// cannot deserialize itself into execution authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentExecutionContext {
    /// Runtime-derived subject of the execution.
    pub actor: ExecutionActor,
    /// Optional logical workspace; `None` means no filesystem authority.
    pub workspace: Option<ExecutionWorkspace>,
    /// Explicit capabilities granted for this execution.
    pub capabilities: BTreeSet<ExecutionCapability>,
    /// Overall tool-operation timeout selected by Runtime.
    pub timeout: Option<Duration>,
    /// Runtime wall-clock timestamp when this execution was admitted.
    pub started_at_unix_secs: i64,
    /// Bounded correlation identifier; it grants no authority.
    pub trace_id: Option<String>,
}

impl AgentExecutionContext {
    /// Construct a context with identity but no workspace or capabilities.
    #[must_use]
    pub fn restricted(actor: ExecutionActor) -> Self {
        Self {
            actor,
            workspace: None,
            capabilities: BTreeSet::new(),
            timeout: None,
            started_at_unix_secs: 0,
            trace_id: None,
        }
    }

    /// Construct a restricted context from already validated identities.
    #[must_use]
    pub fn restricted_for(
        user_id: impl Into<String>,
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self::restricted(ExecutionActor::new(user_id, agent_id, session_id))
    }

    /// Attach the logical workspace already selected by Runtime.
    #[must_use]
    pub fn with_workspace(mut self, workspace: ExecutionWorkspace) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Grant one explicit Agent-understood capability.
    #[must_use]
    pub fn with_capability(mut self, capability: ExecutionCapability) -> Self {
        self.capabilities.insert(capability);
        self
    }

    /// Set the maximum duration available to one operation.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Record Runtime's trusted admission timestamp.
    #[must_use]
    pub const fn with_started_at_unix_secs(mut self, started_at_unix_secs: i64) -> Self {
        self.started_at_unix_secs = started_at_unix_secs;
        self
    }

    /// Attach a correlation identifier without changing authority.
    #[must_use]
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }
}

#[cfg(test)]
#[path = "../tests/unit/execution_context.rs"]
mod tests;
