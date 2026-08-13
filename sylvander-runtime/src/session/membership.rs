//! Pure invariants for multiple first-class Agent instances in one Session.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sylvander_api::{AgentInstanceId, SessionId};

use crate::agent::instance::{AgentInstance, AgentInstanceState};

/// Durable ownership of final Session arbitration.
///
/// Runtime uses the monotonically increasing epoch and fencing token to reject
/// decisions from a moderator that lost its lease during recovery or
/// succession.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGovernance {
    pub session_id: SessionId,
    pub moderator_instance_id: AgentInstanceId,
    pub governance_revision: String,
    pub lease_epoch: u64,
    pub fencing_token: u64,
    pub updated_at: i64,
}

/// Complete first-class Agent membership of one Session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMembership {
    pub session_id: SessionId,
    pub participants: Vec<AgentInstance>,
    pub governance: SessionGovernance,
}

impl SessionMembership {
    /// Construct a membership only when all cross-record invariants hold.
    pub fn new(
        session_id: SessionId,
        participants: Vec<AgentInstance>,
        governance: SessionGovernance,
    ) -> Result<Self, SessionMembershipError> {
        let membership = Self {
            session_id,
            participants,
            governance,
        };
        membership.validate()?;
        Ok(membership)
    }

    /// Validate the exact Session boundary and unique active root moderator.
    pub fn validate(&self) -> Result<(), SessionMembershipError> {
        if self.participants.is_empty() {
            return Err(SessionMembershipError::Empty);
        }
        if self.governance.session_id != self.session_id {
            return Err(SessionMembershipError::GovernanceSessionMismatch);
        }
        if self.governance.governance_revision.trim().is_empty()
            || self.governance.lease_epoch == 0
            || self.governance.fencing_token == 0
        {
            return Err(SessionMembershipError::InvalidGovernanceLease);
        }

        let mut identities = HashSet::with_capacity(self.participants.len());
        let mut moderators = 0_u32;
        let mut governed_moderator = None;
        for participant in &self.participants {
            if participant.session_id != self.session_id {
                return Err(SessionMembershipError::ParticipantSessionMismatch(
                    participant.instance_id.clone(),
                ));
            }
            if participant.definition.revision == 0
                || participant.capability_revision.trim().is_empty()
            {
                return Err(SessionMembershipError::InvalidParticipant(
                    participant.instance_id.clone(),
                ));
            }
            if !identities.insert(participant.instance_id.clone()) {
                return Err(SessionMembershipError::DuplicateParticipant(
                    participant.instance_id.clone(),
                ));
            }
            if participant.role.is_root_moderator() {
                moderators = moderators.saturating_add(1);
            }
            if participant.instance_id == self.governance.moderator_instance_id {
                governed_moderator = Some(participant);
            }
        }

        if moderators != 1 {
            return Err(SessionMembershipError::ModeratorCount(moderators));
        }
        let moderator = governed_moderator.ok_or(SessionMembershipError::ModeratorMissing)?;
        if !moderator.role.is_root_moderator() {
            return Err(SessionMembershipError::ModeratorRoleMismatch);
        }
        if moderator.state.is_terminal()
            || moderator.state == AgentInstanceState::ManualReconciliation
        {
            return Err(SessionMembershipError::ModeratorUnavailable);
        }
        Ok(())
    }

    #[must_use]
    pub fn moderator(&self) -> &AgentInstance {
        self.participants
            .iter()
            .find(|participant| participant.instance_id == self.governance.moderator_instance_id)
            .expect("validated membership always contains its moderator")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionMembershipError {
    #[error("a Session must contain at least one Agent instance")]
    Empty,
    #[error("Session governance belongs to a different Session")]
    GovernanceSessionMismatch,
    #[error("Session governance lease is incomplete")]
    InvalidGovernanceLease,
    #[error("Agent instance {0} belongs to a different Session")]
    ParticipantSessionMismatch(AgentInstanceId),
    #[error("Agent instance {0} has an incomplete frozen definition or capability revision")]
    InvalidParticipant(AgentInstanceId),
    #[error("Agent instance {0} appears more than once")]
    DuplicateParticipant(AgentInstanceId),
    #[error("a Session must contain exactly one root moderator, found {0}")]
    ModeratorCount(u32),
    #[error("the governed moderator is not a Session participant")]
    ModeratorMissing,
    #[error("the governed moderator does not hold the moderator role")]
    ModeratorRoleMismatch,
    #[error("the governed moderator is not available for arbitration")]
    ModeratorUnavailable,
}

#[cfg(test)]
#[path = "../../tests/unit/session_membership.rs"]
mod tests;
