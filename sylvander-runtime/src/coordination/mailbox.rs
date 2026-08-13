//! Durable inter-Agent messages with explicit routing and replay semantics.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sylvander_api::{AgentInstanceId, CoordinationMessageId, SessionId, TaskId};

use crate::coordination::topology::SessionTopology;
use crate::session::membership::SessionMembership;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinationMessageKind {
    Task,
    Progress,
    Evidence,
    Question,
    Decision,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageDeliveryState {
    Pending,
    Claimed,
    Delivered,
    Acknowledged,
    Expired,
    DeadLetter,
}

impl MessageDeliveryState {
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next || matches!(self, Self::Acknowledged | Self::Expired | Self::DeadLetter) {
            return false;
        }
        matches!(
            (self, next),
            (
                Self::Pending,
                Self::Claimed | Self::Expired | Self::DeadLetter
            ) | (
                Self::Claimed,
                Self::Delivered | Self::Expired | Self::DeadLetter
            ) | (
                Self::Delivered,
                Self::Acknowledged | Self::Expired | Self::DeadLetter
            )
        )
    }
}

/// One at-least-once deliverable envelope; `message_id` is its dedupe key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinationMessage {
    pub message_id: CoordinationMessageId,
    pub session_id: SessionId,
    pub sender_instance_id: AgentInstanceId,
    pub recipient_instance_id: AgentInstanceId,
    pub task_id: Option<TaskId>,
    pub kind: CoordinationMessageKind,
    pub payload: String,
    pub topology_revision: u64,
    /// Frozen source-to-recipient route used for audit and loop detection.
    pub route: Vec<AgentInstanceId>,
    pub max_hops: u16,
    pub state: MessageDeliveryState,
    pub delivery_attempts: u32,
    pub revision: u64,
    pub expires_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl CoordinationMessage {
    pub fn validate_new(
        &self,
        topology: &SessionTopology,
        membership: &SessionMembership,
        now: i64,
    ) -> Result<(), MailboxError> {
        if self.state != MessageDeliveryState::Pending
            || self.revision != 0
            || self.delivery_attempts != 0
        {
            return Err(MailboxError::NotNew);
        }
        if self.session_id != membership.session_id
            || topology.session_id != self.session_id
            || topology.membership_revision != membership.governance.membership_revision
            || self.topology_revision != topology.topology_revision
        {
            return Err(MailboxError::StaleFacts);
        }
        if self.payload.trim().is_empty() {
            return Err(MailboxError::EmptyPayload);
        }
        if self.expires_at <= now {
            return Err(MailboxError::Expired);
        }
        let members: HashSet<_> = membership
            .participants
            .iter()
            .map(|participant| &participant.instance_id)
            .collect();
        if !members.contains(&self.sender_instance_id)
            || !members.contains(&self.recipient_instance_id)
        {
            return Err(MailboxError::UnknownActor);
        }
        let unique_route: HashSet<_> = self.route.iter().collect();
        if unique_route.len() != self.route.len() {
            return Err(MailboxError::RouteCycle);
        }
        let hops = self.route.len().saturating_sub(1);
        if self.max_hops == 0 || hops > usize::from(self.max_hops) {
            return Err(MailboxError::HopLimit);
        }
        let expected_route = topology
            .route_between(&self.sender_instance_id, &self.recipient_instance_id)
            .ok_or(MailboxError::Unroutable)?;
        if self.route != expected_route {
            return Err(MailboxError::InvalidRoute);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MailboxError {
    #[error("message is not a new pending delivery")]
    NotNew,
    #[error("message was derived from stale topology or membership facts")]
    StaleFacts,
    #[error("message payload cannot be empty")]
    EmptyPayload,
    #[error("message expired before enqueue")]
    Expired,
    #[error("message references an unknown Agent instance")]
    UnknownActor,
    #[error("message route contains a cycle")]
    RouteCycle,
    #[error("message route exceeds its hop limit")]
    HopLimit,
    #[error("recipient is not reachable in the governed topology")]
    Unroutable,
    #[error("message route is not the governed shortest path")]
    InvalidRoute,
}

#[cfg(test)]
#[path = "../../tests/unit/coordination_mailbox.rs"]
mod tests;
