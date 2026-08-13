//! Pure domain model for one Agent instance participating in a Session.

use serde::{Deserialize, Serialize};
use sylvander_api::{AgentId, AgentInstanceId, DelegationId, SessionId, SwarmId, TaskId};

/// Exact immutable Agent definition resolved for an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefinitionKey {
    pub agent_id: AgentId,
    pub revision: u64,
}

/// Durable reason an Agent instance exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentInstanceOrigin {
    Defined,
    Forked {
        parent_instance_id: AgentInstanceId,
        fork_sequence: u64,
    },
    Delegated {
        parent_instance_id: AgentInstanceId,
        delegation_id: DelegationId,
        task_id: TaskId,
    },
    SwarmMember {
        swarm_id: SwarmId,
    },
}

/// Governed role occupied by an Agent instance inside one Session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionAgentRole {
    Moderator,
    Coordinator { swarm_id: SwarmId },
    Worker,
    Reviewer,
    Specialist,
    Observer,
}

impl SessionAgentRole {
    #[must_use]
    pub const fn is_root_moderator(&self) -> bool {
        matches!(self, Self::Moderator)
    }
}

/// Conversation history visible to one Agent instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryView {
    SharedLane {
        cursor: u64,
    },
    ForkSnapshot {
        base_sequence: u64,
        branch_id: String,
    },
}

/// Where an interaction that cannot be decided automatically is routed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalRoute {
    User,
    Parent { instance_id: AgentInstanceId },
    Moderator { instance_id: AgentInstanceId },
    Guardian,
}

/// Durable lifecycle of a concrete Agent instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentInstanceState {
    Created,
    Ready,
    Running,
    WaitingMessage,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
    ManualReconciliation,
}

impl AgentInstanceState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self.is_terminal() || self == next {
            return false;
        }
        matches!(
            (self, next),
            (Self::Created, Self::Ready | Self::ManualReconciliation)
                | (
                    Self::Ready,
                    Self::Running | Self::Cancelled | Self::ManualReconciliation
                )
                | (
                    Self::Running,
                    Self::WaitingMessage
                        | Self::WaitingApproval
                        | Self::Completed
                        | Self::Failed
                        | Self::Cancelled
                        | Self::ManualReconciliation
                )
                | (
                    Self::WaitingMessage | Self::WaitingApproval,
                    Self::Running | Self::Failed | Self::Cancelled | Self::ManualReconciliation
                )
                | (
                    Self::ManualReconciliation,
                    Self::Ready | Self::Failed | Self::Cancelled
                )
        )
    }
}

/// One first-class Agent participant in a durable Session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInstance {
    pub instance_id: AgentInstanceId,
    pub session_id: SessionId,
    pub definition: AgentDefinitionKey,
    pub origin: AgentInstanceOrigin,
    pub role: SessionAgentRole,
    pub history_view: HistoryView,
    pub approval_route: ApprovalRoute,
    pub state: AgentInstanceState,
    /// Monotonic CAS revision for lifecycle, role, lease, and recovery writes.
    pub lifecycle_revision: u64,
    pub capability_revision: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[cfg(test)]
#[path = "../../../tests/unit/agent_instance.rs"]
mod tests;
