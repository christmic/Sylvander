//! Governed transfer of a durable task between first-class Agent instances.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sylvander_api::{AgentInstanceId, HandoffId, SessionId, TaskId};

use crate::coordination::task::CoordinationTask;
use crate::coordination::topology::SessionTopology;
use crate::session::membership::SessionMembership;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffState {
    Proposed,
    AwaitingArbitration,
    Accepted,
    Rejected,
    Expired,
    Cancelled,
}

impl HandoffState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Accepted | Self::Rejected | Self::Expired | Self::Cancelled
        )
    }

    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self.is_terminal() || self == next {
            return false;
        }
        matches!(
            (self, next),
            (
                Self::Proposed,
                Self::AwaitingArbitration | Self::Cancelled | Self::Expired
            ) | (
                Self::AwaitingArbitration,
                Self::Accepted | Self::Rejected | Self::Cancelled | Self::Expired
            )
        )
    }
}

/// Immutable proposal facts plus CAS fences for every fact used to decide it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskHandoff {
    pub handoff_id: HandoffId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub from_instance_id: AgentInstanceId,
    pub to_instance_id: AgentInstanceId,
    pub requested_by: AgentInstanceId,
    pub arbitrator_instance_id: AgentInstanceId,
    pub task_revision: u64,
    pub topology_revision: u64,
    pub reason: String,
    pub state: HandoffState,
    pub revision: u64,
    pub expires_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl TaskHandoff {
    /// Validate a proposal against exact task, topology, and membership facts.
    pub fn validate_proposal(
        &self,
        task: &CoordinationTask,
        topology: &SessionTopology,
        membership: &SessionMembership,
        now: i64,
    ) -> Result<(), HandoffError> {
        if self.state != HandoffState::Proposed || self.revision != 0 {
            return Err(HandoffError::NotNew);
        }
        if self.session_id != membership.session_id
            || task.session_id != self.session_id
            || task.task_id != self.task_id
        {
            return Err(HandoffError::TaskMismatch);
        }
        if self.task_revision != task.revision
            || self.topology_revision != topology.topology_revision
            || topology.membership_revision != membership.governance.membership_revision
        {
            return Err(HandoffError::StaleFacts);
        }
        if task.state.is_terminal() {
            return Err(HandoffError::TerminalTask);
        }
        if task.assigned_to.as_ref() != Some(&self.from_instance_id) {
            return Err(HandoffError::NotCurrentAssignee);
        }
        if self.from_instance_id == self.to_instance_id {
            return Err(HandoffError::SameAssignee);
        }
        if task.handoff_count >= task.max_handoffs {
            return Err(HandoffError::HandoffBudgetExhausted);
        }
        let members: HashSet<_> = membership
            .participants
            .iter()
            .map(|participant| &participant.instance_id)
            .collect();
        if !members.contains(&self.to_instance_id)
            || !members.contains(&self.requested_by)
            || !members.contains(&self.arbitrator_instance_id)
        {
            return Err(HandoffError::UnknownActor);
        }
        let moderator = &membership.governance.moderator_instance_id;
        if self.requested_by != self.from_instance_id && &self.requested_by != moderator {
            return Err(HandoffError::UnauthorizedRequester);
        }
        let arbitrator =
            topology.arbitrator_for(&self.from_instance_id, &self.to_instance_id, membership);
        if self.arbitrator_instance_id != arbitrator {
            return Err(HandoffError::WrongArbitrator(arbitrator));
        }
        if self.reason.trim().is_empty() {
            return Err(HandoffError::EmptyReason);
        }
        if self.expires_at <= now {
            return Err(HandoffError::Expired);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandoffError {
    #[error("handoff is not a new proposal")]
    NotNew,
    #[error("handoff does not match the durable task")]
    TaskMismatch,
    #[error("handoff was derived from stale task, topology, or membership facts")]
    StaleFacts,
    #[error("a terminal task cannot be handed off")]
    TerminalTask,
    #[error("handoff source is not the current task assignee")]
    NotCurrentAssignee,
    #[error("handoff must change task ownership")]
    SameAssignee,
    #[error("task handoff budget is exhausted")]
    HandoffBudgetExhausted,
    #[error("handoff references an unknown Agent instance")]
    UnknownActor,
    #[error("only the current assignee or Session moderator may request a handoff")]
    UnauthorizedRequester,
    #[error("handoff must be arbitrated by {0}")]
    WrongArbitrator(AgentInstanceId),
    #[error("handoff reason cannot be empty")]
    EmptyReason,
    #[error("handoff proposal has expired")]
    Expired,
}

#[cfg(test)]
#[path = "../../tests/unit/task_handoff.rs"]
mod tests;
