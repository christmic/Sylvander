//! Durable Agent-specific workspace views and fenced write ownership.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sylvander_api::{AgentInstanceId, SessionId, WorkspaceIntegrationId, WorkspaceViewId};

use crate::agent::instance::AgentInstanceState;
use crate::session::membership::SessionMembership;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceIsolation {
    Shared,
    IsolatedWorktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceViewState {
    Provisioning,
    Active,
    Integrating,
    Integrated,
    Conflicted,
    Released,
    ManualReconciliation,
}

impl WorkspaceViewState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Integrated | Self::Released)
    }

    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next || self.is_terminal() {
            return false;
        }
        matches!(
            (self, next),
            (
                Self::Provisioning,
                Self::Active | Self::Released | Self::ManualReconciliation
            ) | (
                Self::Active | Self::Conflicted,
                Self::Integrating | Self::Released | Self::ManualReconciliation
            ) | (
                Self::Integrating,
                Self::Integrated | Self::Conflicted | Self::ManualReconciliation
            ) | (Self::ManualReconciliation, Self::Released)
        )
    }
}

/// One durable mount visible to one concrete Agent instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceView {
    pub view_id: WorkspaceViewId,
    pub session_id: SessionId,
    pub agent_instance_id: AgentInstanceId,
    pub membership_revision: u64,
    pub access: WorkspaceAccess,
    pub isolation: WorkspaceIsolation,
    pub source_workspace: PathBuf,
    pub effective_workspace: PathBuf,
    pub target_id: Option<String>,
    pub branch: Option<String>,
    pub base_revision: Option<String>,
    pub state: WorkspaceViewState,
    pub lease_epoch: u64,
    pub fencing_token: u64,
    pub revision: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl AgentWorkspaceView {
    pub fn validate_new(&self, membership: &SessionMembership) -> Result<(), WorkspaceViewError> {
        if self.state != WorkspaceViewState::Provisioning
            || self.revision != 0
            || self.lease_epoch == 0
            || self.fencing_token == 0
        {
            return Err(WorkspaceViewError::NotNew);
        }
        if self.session_id != membership.session_id
            || self.membership_revision != membership.governance.membership_revision
        {
            return Err(WorkspaceViewError::StaleMembership);
        }
        let instance = membership
            .participants
            .iter()
            .find(|participant| participant.instance_id == self.agent_instance_id)
            .ok_or(WorkspaceViewError::UnknownAgent)?;
        if instance.state.is_terminal()
            || instance.state == AgentInstanceState::ManualReconciliation
        {
            return Err(WorkspaceViewError::UnavailableAgent);
        }
        if self.source_workspace.as_os_str().is_empty()
            || self.effective_workspace.as_os_str().is_empty()
            || self.source_workspace.to_str().is_none()
            || self.effective_workspace.to_str().is_none()
            || self
                .target_id
                .as_ref()
                .is_some_and(|target| target.trim().is_empty())
        {
            return Err(WorkspaceViewError::InvalidLocation);
        }
        match (self.access, self.isolation) {
            (WorkspaceAccess::ReadWrite, WorkspaceIsolation::Shared) => {
                return Err(WorkspaceViewError::SharedWriteForbidden);
            }
            (WorkspaceAccess::ReadWrite, WorkspaceIsolation::IsolatedWorktree) => {
                if self.source_workspace == self.effective_workspace
                    || self
                        .branch
                        .as_ref()
                        .is_none_or(|branch| branch.trim().is_empty())
                    || self
                        .base_revision
                        .as_ref()
                        .is_none_or(|revision| revision.trim().is_empty())
                {
                    return Err(WorkspaceViewError::IncompleteWorktree);
                }
            }
            (WorkspaceAccess::ReadOnly, WorkspaceIsolation::Shared) => {
                if self.source_workspace != self.effective_workspace
                    || self.branch.is_some()
                    || self.base_revision.is_some()
                {
                    return Err(WorkspaceViewError::InvalidSharedView);
                }
            }
            (WorkspaceAccess::ReadOnly, WorkspaceIsolation::IsolatedWorktree) => {
                if self.source_workspace == self.effective_workspace
                    || self
                        .branch
                        .as_ref()
                        .is_none_or(|branch| branch.trim().is_empty())
                    || self
                        .base_revision
                        .as_ref()
                        .is_none_or(|revision| revision.trim().is_empty())
                {
                    return Err(WorkspaceViewError::IncompleteWorktree);
                }
            }
        }
        Ok(())
    }

    pub fn transition(
        &mut self,
        expected_revision: u64,
        lease_epoch: u64,
        fencing_token: u64,
        next: WorkspaceViewState,
        now: i64,
    ) -> Result<(), WorkspaceViewError> {
        if self.revision != expected_revision {
            return Err(WorkspaceViewError::RevisionConflict);
        }
        if self.lease_epoch != lease_epoch || self.fencing_token != fencing_token {
            return Err(WorkspaceViewError::StaleLease);
        }
        if !self.state.can_transition_to(next) {
            return Err(WorkspaceViewError::InvalidTransition);
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(WorkspaceViewError::RevisionOverflow)?;
        self.state = next;
        self.updated_at = now;
        Ok(())
    }
}

/// Immutable moderator approval for one exact reviewed workspace revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceIntegrationApproval {
    pub integration_id: WorkspaceIntegrationId,
    pub view_id: WorkspaceViewId,
    pub session_id: SessionId,
    pub agent_instance_id: AgentInstanceId,
    pub approved_by: AgentInstanceId,
    pub membership_revision: u64,
    pub topology_revision: u64,
    pub view_revision: u64,
    pub lease_epoch: u64,
    pub fencing_token: u64,
    pub review_digest: String,
    pub approved_at: i64,
}

impl WorkspaceIntegrationApproval {
    pub fn validate(
        &self,
        view: &AgentWorkspaceView,
        membership: &SessionMembership,
        topology_revision: u64,
    ) -> Result<(), WorkspaceViewError> {
        if self.view_id != view.view_id
            || self.session_id != view.session_id
            || self.agent_instance_id != view.agent_instance_id
        {
            return Err(WorkspaceViewError::ApprovalMismatch);
        }
        if self.approved_by != membership.governance.moderator_instance_id {
            return Err(WorkspaceViewError::UnauthorizedIntegrator);
        }
        if self.membership_revision != membership.governance.membership_revision
            || self.membership_revision != view.membership_revision
            || self.topology_revision != topology_revision
            || self.view_revision != view.revision
            || self.lease_epoch != view.lease_epoch
            || self.fencing_token != view.fencing_token
        {
            return Err(WorkspaceViewError::StaleIntegrationApproval);
        }
        if !matches!(
            view.state,
            WorkspaceViewState::Active | WorkspaceViewState::Conflicted
        ) || self.review_digest.trim().is_empty()
        {
            return Err(WorkspaceViewError::InvalidIntegrationApproval);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceIntegrationState {
    Approved,
    Applying,
    Applied,
    Conflicted,
    ManualReconciliation,
}

impl WorkspaceIntegrationState {
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next
            || matches!(
                self,
                Self::Applied | Self::Conflicted | Self::ManualReconciliation
            )
        {
            return false;
        }
        matches!(
            (self, next),
            (Self::Approved, Self::Applying | Self::ManualReconciliation)
                | (
                    Self::Applying,
                    Self::Applied | Self::Conflicted | Self::ManualReconciliation
                )
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceIntegration {
    pub approval: WorkspaceIntegrationApproval,
    pub state: WorkspaceIntegrationState,
    pub revision: u64,
    pub updated_at: i64,
}

impl WorkspaceIntegration {
    pub fn new(
        approval: WorkspaceIntegrationApproval,
        view: &AgentWorkspaceView,
        membership: &SessionMembership,
        topology_revision: u64,
    ) -> Result<Self, WorkspaceViewError> {
        approval.validate(view, membership, topology_revision)?;
        let updated_at = approval.approved_at;
        Ok(Self {
            approval,
            state: WorkspaceIntegrationState::Approved,
            revision: 0,
            updated_at,
        })
    }

    pub fn transition(
        &mut self,
        expected_revision: u64,
        next: WorkspaceIntegrationState,
        now: i64,
    ) -> Result<(), WorkspaceViewError> {
        if self.revision != expected_revision {
            return Err(WorkspaceViewError::RevisionConflict);
        }
        if !self.state.can_transition_to(next) {
            return Err(WorkspaceViewError::InvalidIntegrationTransition);
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(WorkspaceViewError::RevisionOverflow)?;
        self.state = next;
        self.updated_at = now;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceViewError {
    #[error("workspace view is not a new fenced lease")]
    NotNew,
    #[error("workspace view is not synchronized to Session membership")]
    StaleMembership,
    #[error("workspace view references an unknown Agent instance")]
    UnknownAgent,
    #[error("workspace view owner is unavailable")]
    UnavailableAgent,
    #[error("workspace view has an invalid location or target")]
    InvalidLocation,
    #[error("writable Agents must use isolated worktrees")]
    SharedWriteForbidden,
    #[error("isolated worktree view lacks branch, base revision, or distinct path")]
    IncompleteWorktree,
    #[error("shared read-only view must exactly match the source workspace")]
    InvalidSharedView,
    #[error("workspace view revision changed")]
    RevisionConflict,
    #[error("workspace view lease was superseded")]
    StaleLease,
    #[error("workspace view lifecycle transition is invalid")]
    InvalidTransition,
    #[error("workspace view revision overflow")]
    RevisionOverflow,
    #[error("workspace integration approval targets a different view")]
    ApprovalMismatch,
    #[error("only the Session moderator may approve final workspace integration")]
    UnauthorizedIntegrator,
    #[error("workspace integration approval was derived from stale facts")]
    StaleIntegrationApproval,
    #[error("workspace integration approval has no reviewed candidate or invalid state")]
    InvalidIntegrationApproval,
    #[error("workspace integration lifecycle transition is invalid")]
    InvalidIntegrationTransition,
}

#[cfg(test)]
#[path = "../../tests/unit/coordination_workspace.rs"]
mod tests;
