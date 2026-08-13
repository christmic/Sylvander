//! Stable public identities shared by API domains.
//!
//! These string newtypes prevent accidental interchange between Agent
//! definitions, concrete Agent instances, Sessions, tasks, and users while
//! preserving compact wire shapes.

use serde::{Deserialize, Serialize};

// ===========================================================================
// ID types
// ===========================================================================

/// Unique identifier for an agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for AgentId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}
impl From<String> for AgentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Unique identifier for one concrete running instance of an Agent definition.
///
/// Multiple instances may resolve the same [`AgentId`] revision inside one
/// Session. Runtime authorization, messaging, turns, leases, and recovery use
/// this identity rather than the reusable definition identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentInstanceId(pub String);

impl AgentInstanceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for AgentInstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for AgentInstanceId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for AgentInstanceId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Stable identity for one governed delegation between Agent instances.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DelegationId(pub String);

impl DelegationId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// Stable identity for one governed task ownership transfer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HandoffId(pub String);

impl HandoffId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// Stable identity and deduplication key for one inter-Agent message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoordinationMessageId(pub String);

impl CoordinationMessageId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// Stable identity for one moderator arbitration case.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GovernanceCaseId(pub String);

impl GovernanceCaseId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// Stable identity for one Agent-specific workspace view and its lease.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceViewId(pub String);

impl WorkspaceViewId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// Stable identity for one durable unit of Agent work.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Stable identity for one governed group of Agent instances.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SwarmId(pub String);

impl SwarmId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// Unique identifier for a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for SessionId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Unique identifier for a human user.
///
/// Distinct from `AgentId` (the LLM-driven runtime) and `SessionId`
/// (a single conversation). One user may own many agents and run many
/// sessions; one session is bound to exactly one user; one agent is
/// owned by exactly one user.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UserId(pub String);

impl UserId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Sentinel for system-originated actions (cron, internal tasks)
    /// that have no real user. Distinct from any real `UserId`.
    pub fn system() -> Self {
        Self("__system__".to_string())
    }
}

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for UserId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for UserId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[cfg(test)]
#[path = "../tests/unit/identity.rs"]
mod tests;
