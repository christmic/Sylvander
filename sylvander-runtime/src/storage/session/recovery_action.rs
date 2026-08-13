//! Durable, idempotent moderator decisions for uncertain execution effects.

use std::fmt;

use serde::{Deserialize, Serialize};
use sylvander_api::{AgentInstanceId, SessionId};
use uuid::Uuid;

use super::{ModelInvocationId, ToolInvocationId};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionRecoveryActionId(String);

impl ExecutionRecoveryActionId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn parse(value: String) -> Result<Self, uuid::Error> {
        Uuid::parse_str(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ExecutionRecoveryActionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ExecutionRecoveryActionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionRecoveryActionTarget {
    Model { invocation_id: ModelInvocationId },
    Tool { invocation_id: ToolInvocationId },
}

impl ExecutionRecoveryActionTarget {
    #[must_use]
    pub fn invocation_id(&self) -> &str {
        match self {
            Self::Model { invocation_id } => invocation_id.as_str(),
            Self::Tool { invocation_id } => invocation_id.as_str(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Model { .. } => "model",
            Self::Tool { .. } => "tool",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRecoveryAction {
    /// The operator accepts an uncertain outcome and releases the Session
    /// without inventing a successful provider response or tool result.
    AbandonTurn,
    /// The operator has independently established that no external effect
    /// occurred, so the same stable tool invocation may execute again.
    ConfirmNoEffectAndRetry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRecoveryActionWrite {
    pub action_id: ExecutionRecoveryActionId,
    pub session_id: SessionId,
    pub turn_id: String,
    pub target: ExecutionRecoveryActionTarget,
    pub expected_ledger_revision: u64,
    pub action: ExecutionRecoveryAction,
    pub resolved_by: AgentInstanceId,
    pub rationale_digest: String,
    pub observed_at: i64,
    pub lease_expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRecoveryActionReceipt {
    pub action_id: ExecutionRecoveryActionId,
    pub session_id: SessionId,
    pub turn_id: String,
    pub target: ExecutionRecoveryActionTarget,
    pub action: ExecutionRecoveryAction,
    pub resolved_by: AgentInstanceId,
    pub outcome_ledger_revision: u64,
    pub recorded_at: i64,
}
