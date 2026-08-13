//! Effect-sandwiched provisioning and recovery of Agent worktree views.

use std::path::Path;
use std::sync::Arc;

use sylvander_api::{AgentInstanceId, WorkspaceViewId};

use crate::coordination::workspace::{
    AgentWorkspaceView, WorkspaceAccess, WorkspaceIsolation, WorkspaceViewState,
};
use crate::session::membership::SessionMembership;
use crate::storage::session::SessionStoreError;
use crate::storage::workspace_coordination::AgentWorkspaceStore;
use crate::workspace::coding::{CodingWorkspaceLease, CodingWorktreeService};

pub struct AgentWorkspaceCoordinator {
    worktrees: Arc<CodingWorktreeService>,
    store: Arc<dyn AgentWorkspaceStore>,
}

impl AgentWorkspaceCoordinator {
    #[must_use]
    pub fn new(worktrees: Arc<CodingWorktreeService>, store: Arc<dyn AgentWorkspaceStore>) -> Self {
        Self { worktrees, store }
    }

    /// Provision an isolated worktree and persist its exact receipt.
    ///
    /// The worktree manager durably writes its manifest before this service
    /// writes the Runtime view. An orphan manifest has no replay authority and
    /// is removed by normal worktree reconciliation. Once the view exists,
    /// `Provisioning` is a recoverable execution position.
    #[allow(clippy::too_many_arguments)]
    pub async fn provision(
        &self,
        view_id: WorkspaceViewId,
        membership: &SessionMembership,
        agent_instance_id: AgentInstanceId,
        access: WorkspaceAccess,
        target_id: &str,
        requested_workspace: &Path,
        lease_epoch: u64,
        fencing_token: u64,
        now: i64,
    ) -> Result<AgentWorkspaceView, AgentWorkspaceCoordinatorError> {
        let lease = self
            .worktrees
            .create(&view_id.0, target_id, requested_workspace)
            .await
            .map_err(AgentWorkspaceCoordinatorError::Worktree)?
            .ok_or_else(|| {
                AgentWorkspaceCoordinatorError::Worktree(
                    "writable Agent workspace requires a Git worktree".into(),
                )
            })?;
        let view = workspace_view(
            view_id,
            membership,
            agent_instance_id,
            access,
            requested_workspace,
            &lease,
            lease_epoch,
            fencing_token,
            now,
        );
        if let Err(source) = self.store.create_workspace_view(&view, membership).await {
            let cleanup = self
                .worktrees
                .discard_if_present(&view.view_id.0, lease.target_id.as_deref())
                .await;
            return match cleanup {
                Ok(()) => Err(AgentWorkspaceCoordinatorError::Store(source)),
                Err(cleanup) => {
                    Err(AgentWorkspaceCoordinatorError::Compensation { source, cleanup })
                }
            };
        }
        self.activate(&view).await
    }

    /// Resume only from a durable `Provisioning` fact and an exact receipt.
    pub async fn recover_provisioning(
        &self,
        view: &AgentWorkspaceView,
    ) -> Result<AgentWorkspaceView, AgentWorkspaceCoordinatorError> {
        if view.state != WorkspaceViewState::Provisioning {
            return Err(AgentWorkspaceCoordinatorError::InvalidRecoveryPosition);
        }
        let receipt = self
            .worktrees
            .open(&view.view_id.0, view.target_id.as_deref())
            .await
            .map_err(AgentWorkspaceCoordinatorError::Worktree)?;
        if !receipt_matches(view, &receipt) {
            return Err(AgentWorkspaceCoordinatorError::ReceiptMismatch);
        }
        self.activate(view).await
    }

    async fn activate(
        &self,
        view: &AgentWorkspaceView,
    ) -> Result<AgentWorkspaceView, AgentWorkspaceCoordinatorError> {
        let activated_at = crate::session::now_secs().max(view.updated_at);
        self.store
            .transition_workspace_view(
                &view.view_id,
                view.revision,
                view.lease_epoch,
                view.fencing_token,
                WorkspaceViewState::Active,
                activated_at,
            )
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)
    }
}

#[allow(clippy::too_many_arguments)]
fn workspace_view(
    view_id: WorkspaceViewId,
    membership: &SessionMembership,
    agent_instance_id: AgentInstanceId,
    access: WorkspaceAccess,
    requested_workspace: &Path,
    lease: &CodingWorkspaceLease,
    lease_epoch: u64,
    fencing_token: u64,
    now: i64,
) -> AgentWorkspaceView {
    AgentWorkspaceView {
        view_id,
        session_id: membership.session_id.clone(),
        agent_instance_id,
        membership_revision: membership.governance.membership_revision,
        access,
        isolation: WorkspaceIsolation::IsolatedWorktree,
        source_workspace: requested_workspace.to_owned(),
        effective_workspace: lease.effective_workspace.clone(),
        target_id: lease.target_id.clone(),
        branch: Some(lease.branch.clone()),
        base_revision: Some(lease.base_revision.clone()),
        state: WorkspaceViewState::Provisioning,
        lease_epoch,
        fencing_token,
        revision: 0,
        created_at: now,
        updated_at: now,
    }
}

fn receipt_matches(view: &AgentWorkspaceView, receipt: &CodingWorkspaceLease) -> bool {
    view.effective_workspace == receipt.effective_workspace
        && view.target_id == receipt.target_id
        && view.branch.as_ref() == Some(&receipt.branch)
        && view.base_revision.as_ref() == Some(&receipt.base_revision)
}

#[derive(Debug, thiserror::Error)]
pub enum AgentWorkspaceCoordinatorError {
    #[error("worktree operation failed: {0}")]
    Worktree(String),
    #[error("workspace view persistence failed: {0}")]
    Store(SessionStoreError),
    #[error("workspace persistence failed ({source}); cleanup also failed: {cleanup}")]
    Compensation {
        source: SessionStoreError,
        cleanup: String,
    },
    #[error("workspace recovery requires a durable provisioning position")]
    InvalidRecoveryPosition,
    #[error("workspace worktree receipt does not match its durable view")]
    ReceiptMismatch,
}

#[cfg(test)]
#[path = "../../tests/unit/agent_workspace_coordinator.rs"]
mod tests;
