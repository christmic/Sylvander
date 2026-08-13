//! Effect-sandwiched provisioning and recovery of Agent worktree views.

use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use sylvander_api::{AgentInstanceId, SessionId, WorkspaceIntegrationId, WorkspaceViewId};

use crate::coordination::workspace::{
    AgentWorkspaceView, WorkspaceAccess, WorkspaceIntegration, WorkspaceIntegrationApproval,
    WorkspaceIntegrationState, WorkspaceIsolation, WorkspaceViewState,
};
use crate::session::membership::SessionMembership;
use crate::storage::session::SessionStoreError;
use crate::storage::workspace_coordination::AgentWorkspaceStore;
use crate::workspace::coding::{
    CodingWorkspaceLease, CodingWorktreeService, WorkspaceMergePosition,
};
use crate::workspace::local::WorkspaceDiff;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceReview {
    pub view_id: WorkspaceViewId,
    pub session_id: SessionId,
    pub agent_instance_id: AgentInstanceId,
    pub view_revision: u64,
    pub target_revision: String,
    pub candidate_revision: String,
    pub review_digest: String,
    pub diff: WorkspaceDiff,
}

pub struct AgentWorkspaceCoordinator {
    worktrees: Arc<CodingWorktreeService>,
    store: Arc<dyn AgentWorkspaceStore>,
}

impl AgentWorkspaceCoordinator {
    #[must_use]
    pub fn new(worktrees: Arc<CodingWorktreeService>, store: Arc<dyn AgentWorkspaceStore>) -> Self {
        Self { worktrees, store }
    }

    pub async fn view(
        &self,
        view_id: &WorkspaceViewId,
    ) -> Result<Option<AgentWorkspaceView>, AgentWorkspaceCoordinatorError> {
        self.store
            .workspace_view(view_id)
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)
    }

    pub async fn integration(
        &self,
        integration_id: &WorkspaceIntegrationId,
    ) -> Result<Option<WorkspaceIntegration>, AgentWorkspaceCoordinatorError> {
        self.store
            .workspace_integration(integration_id)
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)
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
            .create(
                &workspace_lease_id(&view_id),
                target_id,
                requested_workspace,
            )
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
                .discard_if_present(
                    &workspace_lease_id(&view.view_id),
                    lease.target_id.as_deref(),
                )
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

    /// Provision or recover the deterministic isolated view for one Agent spawn.
    #[allow(clippy::too_many_arguments)]
    pub async fn ensure_isolated(
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
        if let Some(existing) = self
            .store
            .workspace_view(&view_id)
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)?
        {
            if existing.agent_instance_id != agent_instance_id
                || existing.source_workspace != requested_workspace
                || existing.access != access
                || existing.isolation != WorkspaceIsolation::IsolatedWorktree
                || (self.worktrees.is_remote_target(target_id)
                    && existing.target_id.as_deref() != Some(target_id))
                || (!self.worktrees.is_remote_target(target_id) && existing.target_id.is_some())
            {
                return Err(AgentWorkspaceCoordinatorError::ReceiptMismatch);
            }
            return match existing.state {
                WorkspaceViewState::Provisioning => self.recover_provisioning(&existing).await,
                WorkspaceViewState::Active => Ok(existing),
                _ => Err(AgentWorkspaceCoordinatorError::InvalidRecoveryPosition),
            };
        }
        self.provision(
            view_id,
            membership,
            agent_instance_id,
            access,
            target_id,
            requested_workspace,
            lease_epoch,
            fencing_token,
            now,
        )
        .await
    }

    /// Persist an idempotent read-only shared view without creating a worktree.
    #[allow(clippy::too_many_arguments)]
    pub async fn ensure_shared_read_only(
        &self,
        view_id: WorkspaceViewId,
        membership: &SessionMembership,
        agent_instance_id: AgentInstanceId,
        workspace: &Path,
        lease_epoch: u64,
        fencing_token: u64,
        now: i64,
    ) -> Result<AgentWorkspaceView, AgentWorkspaceCoordinatorError> {
        if let Some(existing) = self
            .store
            .workspace_view(&view_id)
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)?
        {
            if existing.agent_instance_id == agent_instance_id
                && existing.source_workspace == workspace
                && existing.effective_workspace == workspace
                && existing.access == WorkspaceAccess::ReadOnly
                && existing.isolation == WorkspaceIsolation::Shared
                && existing.state == WorkspaceViewState::Active
            {
                return Ok(existing);
            }
            return Err(AgentWorkspaceCoordinatorError::ReceiptMismatch);
        }
        let view = AgentWorkspaceView {
            view_id,
            session_id: membership.session_id.clone(),
            agent_instance_id,
            membership_revision: membership.governance.membership_revision,
            access: WorkspaceAccess::ReadOnly,
            isolation: WorkspaceIsolation::Shared,
            source_workspace: workspace.to_owned(),
            effective_workspace: workspace.to_owned(),
            target_id: None,
            branch: None,
            base_revision: None,
            state: WorkspaceViewState::Provisioning,
            lease_epoch,
            fencing_token,
            revision: 0,
            created_at: now,
            updated_at: now,
        };
        self.store
            .create_workspace_view(&view, membership)
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)?;
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
            .open(
                &workspace_lease_id(&view.view_id),
                view.target_id.as_deref(),
            )
            .await
            .map_err(AgentWorkspaceCoordinatorError::Worktree)?;
        if !receipt_matches(view, &receipt) {
            return Err(AgentWorkspaceCoordinatorError::ReceiptMismatch);
        }
        self.activate(view).await
    }

    /// Freeze one candidate commit and return the exact review evidence.
    pub async fn prepare_review(
        &self,
        view_id: &WorkspaceViewId,
    ) -> Result<WorkspaceReview, AgentWorkspaceCoordinatorError> {
        let view = self
            .store
            .workspace_view(view_id)
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)?
            .ok_or(AgentWorkspaceCoordinatorError::MissingWorkspaceView)?;
        if !matches!(
            view.state,
            WorkspaceViewState::Active | WorkspaceViewState::Conflicted
        ) {
            return Err(AgentWorkspaceCoordinatorError::InvalidReviewPosition);
        }
        let prepared = self
            .worktrees
            .prepare_integration(
                &workspace_lease_id(&view.view_id),
                view.target_id.as_deref(),
            )
            .await
            .map_err(AgentWorkspaceCoordinatorError::Worktree)?;
        let review_digest = review_digest(&view, &prepared);
        Ok(WorkspaceReview {
            view_id: view.view_id,
            session_id: view.session_id,
            agent_instance_id: view.agent_instance_id,
            view_revision: view.revision,
            target_revision: prepared.target_revision,
            candidate_revision: prepared.candidate_revision,
            review_digest,
            diff: prepared.diff,
        })
    }

    /// Recompute and persist moderator approval for one exact reviewed commit.
    #[allow(clippy::too_many_arguments)]
    pub async fn approve_integration(
        &self,
        integration_id: WorkspaceIntegrationId,
        view_id: &WorkspaceViewId,
        expected_review_digest: &str,
        approved_by: AgentInstanceId,
        membership: &SessionMembership,
        topology_revision: u64,
        now: i64,
    ) -> Result<WorkspaceIntegration, AgentWorkspaceCoordinatorError> {
        let review = self.prepare_review(view_id).await?;
        if review.review_digest != expected_review_digest {
            return Err(AgentWorkspaceCoordinatorError::ReviewChanged);
        }
        let view = self
            .store
            .workspace_view(view_id)
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)?
            .ok_or(AgentWorkspaceCoordinatorError::MissingWorkspaceView)?;
        let approval = WorkspaceIntegrationApproval {
            integration_id,
            view_id: review.view_id,
            session_id: review.session_id,
            agent_instance_id: review.agent_instance_id,
            approved_by,
            membership_revision: membership.governance.membership_revision,
            topology_revision,
            view_revision: review.view_revision,
            lease_epoch: view.lease_epoch,
            fencing_token: view.fencing_token,
            review_digest: review.review_digest,
            target_revision: review.target_revision,
            candidate_revision: review.candidate_revision,
            approved_at: now,
        };
        let integration = WorkspaceIntegration::new(approval, &view, membership, topology_revision)
            .map_err(|error| AgentWorkspaceCoordinatorError::Approval(error.to_string()))?;
        self.store
            .create_workspace_integration(&integration, &view, membership, topology_revision)
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)?;
        Ok(integration)
    }

    /// Apply or resume one approved integration using the same durable lease.
    pub async fn apply_integration(
        &self,
        integration_id: &sylvander_api::WorkspaceIntegrationId,
        now: i64,
    ) -> Result<WorkspaceIntegrationOutcome, AgentWorkspaceCoordinatorError> {
        let mut integration = self
            .store
            .workspace_integration(integration_id)
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)?
            .ok_or(AgentWorkspaceCoordinatorError::MissingIntegration)?;
        let mut view = self
            .store
            .workspace_view(&integration.approval.view_id)
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)?
            .ok_or(AgentWorkspaceCoordinatorError::MissingWorkspaceView)?;
        match (integration.state, view.state) {
            (
                WorkspaceIntegrationState::Approved,
                WorkspaceViewState::Active | WorkspaceViewState::Conflicted,
            ) => {
                (integration, view) = self
                    .store
                    .advance_workspace_integration(
                        integration_id,
                        integration.revision,
                        view.revision,
                        view.lease_epoch,
                        view.fencing_token,
                        WorkspaceIntegrationState::Applying,
                        WorkspaceViewState::Integrating,
                        None,
                        now,
                    )
                    .await
                    .map_err(AgentWorkspaceCoordinatorError::Store)?;
            }
            (WorkspaceIntegrationState::Applying, WorkspaceViewState::Integrating) => {}
            _ => return Err(AgentWorkspaceCoordinatorError::InvalidIntegrationPosition),
        }
        let lease_id = workspace_lease_id(&view.view_id);
        let position = self
            .worktrees
            .integration_position(
                &lease_id,
                view.target_id.as_deref(),
                &integration.approval.target_revision,
                &integration.approval.candidate_revision,
            )
            .await
            .map_err(AgentWorkspaceCoordinatorError::Worktree)?;
        let merge = match position {
            WorkspaceMergePosition::Ready => {
                self.worktrees
                    .merge_integration(
                        &lease_id,
                        view.target_id.as_deref(),
                        &integration.approval.target_revision,
                        &integration.approval.candidate_revision,
                    )
                    .await
            }
            WorkspaceMergePosition::Applied { merge_revision } => Ok(merge_revision),
            WorkspaceMergePosition::Diverged => {
                Err("workspace integration target diverged after approval".into())
            }
        };
        let finished_at = crate::session::now_secs().max(now);
        let (next_integration, next_view) = if merge.is_ok() {
            (
                WorkspaceIntegrationState::Applied,
                WorkspaceViewState::Integrated,
            )
        } else {
            (
                WorkspaceIntegrationState::Conflicted,
                WorkspaceViewState::Conflicted,
            )
        };
        let merge_revision = merge.as_ref().ok().cloned();
        let (finished, _) = self
            .store
            .advance_workspace_integration(
                integration_id,
                integration.revision,
                view.revision,
                view.lease_epoch,
                view.fencing_token,
                next_integration,
                next_view,
                merge_revision,
                finished_at,
            )
            .await
            .map_err(AgentWorkspaceCoordinatorError::Store)?;
        match merge {
            Ok(_) => Ok(WorkspaceIntegrationOutcome::Applied(finished)),
            Err(reason) => Ok(WorkspaceIntegrationOutcome::Conflicted {
                integration: finished,
                reason,
            }),
        }
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

fn review_digest(
    view: &AgentWorkspaceView,
    prepared: &crate::workspace::coding::PreparedWorkspaceChange,
) -> String {
    let mut digest = Sha256::new();
    for value in [
        "sylvander-workspace-review-v1",
        view.view_id.0.as_str(),
        view.session_id.0.as_str(),
        view.agent_instance_id.0.as_str(),
        prepared.target_revision.as_str(),
        prepared.candidate_revision.as_str(),
        prepared.diff.status.as_str(),
        prepared.diff.patch.as_str(),
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    digest.update(view.revision.to_be_bytes());
    format!("sha256:{:x}", digest.finalize())
}

/// Derive the stable worktree key for a durable logical view identifier.
///
/// Logical identifiers may contain topology-friendly separators such as `:`;
/// Git branch names accept a narrower alphabet in the worktree backends.
pub(crate) fn workspace_lease_id(view_id: &WorkspaceViewId) -> String {
    if !view_id.0.is_empty()
        && view_id
            .0
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return view_id.0.clone();
    }
    format!("view-{:x}", Sha256::digest(view_id.0.as_bytes()))
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
    #[error("workspace view does not exist")]
    MissingWorkspaceView,
    #[error("workspace view is not ready for review")]
    InvalidReviewPosition,
    #[error("workspace review changed before approval")]
    ReviewChanged,
    #[error("workspace integration approval is invalid: {0}")]
    Approval(String),
    #[error("workspace integration does not exist")]
    MissingIntegration,
    #[error("workspace integration is not at a recoverable execution position")]
    InvalidIntegrationPosition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceIntegrationOutcome {
    Applied(WorkspaceIntegration),
    Conflicted {
        integration: WorkspaceIntegration,
        reason: String,
    },
}

#[cfg(test)]
#[path = "../../tests/unit/agent_workspace_coordinator.rs"]
mod tests;
