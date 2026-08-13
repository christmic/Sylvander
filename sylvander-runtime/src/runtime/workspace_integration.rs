use sylvander_api::{AgentInstanceId, WorkspaceIntegrationId, WorkspaceViewId};

use super::{Runtime, RuntimeError};
use crate::agent::run::AuthenticatedSession;
use crate::coordination::topology::AgentRelationKind;
use crate::coordination::workspace::WorkspaceIntegration;
use crate::workspace::agent_views::{WorkspaceIntegrationOutcome, WorkspaceReview};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproveAgentWorkspaceRequest {
    pub integration_id: WorkspaceIntegrationId,
    pub view_id: WorkspaceViewId,
    pub expected_review_digest: String,
}

impl Runtime {
    /// Freeze and inspect the exact candidate commit visible to this Agent,
    /// its moderator, or an explicitly related reviewer.
    pub async fn prepare_agent_workspace_review(
        &self,
        actor: &AuthenticatedSession,
        view_id: &WorkspaceViewId,
    ) -> Result<WorkspaceReview, RuntimeError> {
        let coordinator = self.workspace_coordinator()?;
        let view = coordinator
            .view(view_id)
            .await
            .map_err(workspace_error)?
            .ok_or_else(|| RuntimeError::Coordination("workspace view does not exist".into()))?;
        super::validate_coordination_actor(actor, &view.session_id, actor.agent_instance_id())?;
        self.authorize_workspace_reviewer(actor.agent_instance_id(), &view)
            .await?;
        coordinator
            .prepare_review(view_id)
            .await
            .map_err(workspace_error)
    }

    /// Persist moderator approval only if Runtime recomputes the same review.
    pub async fn approve_agent_workspace(
        &self,
        actor: &AuthenticatedSession,
        request: ApproveAgentWorkspaceRequest,
    ) -> Result<WorkspaceIntegration, RuntimeError> {
        let coordinator = self.workspace_coordinator()?;
        let view = coordinator
            .view(&request.view_id)
            .await
            .map_err(workspace_error)?
            .ok_or_else(|| RuntimeError::Coordination("workspace view does not exist".into()))?;
        let membership = self
            .storage
            .sessions()
            .session_membership(&view.session_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .ok_or_else(|| RuntimeError::Coordination("workspace membership disappeared".into()))?;
        super::validate_coordination_actor(
            actor,
            &view.session_id,
            &membership.governance.moderator_instance_id,
        )?;
        let topology = self
            .coordination_service()?
            .topology(&view.session_id)
            .await
            .map_err(super::coordination_error)?;
        coordinator
            .approve_integration(
                request.integration_id,
                &request.view_id,
                &request.expected_review_digest,
                actor.agent_instance_id().clone(),
                &membership,
                topology.topology_revision,
                crate::session::now_secs(),
            )
            .await
            .map_err(workspace_error)
    }

    /// Apply or recover an exact approved merge under moderator authority.
    pub async fn apply_agent_workspace(
        &self,
        actor: &AuthenticatedSession,
        integration_id: &WorkspaceIntegrationId,
    ) -> Result<WorkspaceIntegrationOutcome, RuntimeError> {
        let coordinator = self.workspace_coordinator()?;
        let integration = coordinator
            .integration(integration_id)
            .await
            .map_err(workspace_error)?
            .ok_or_else(|| {
                RuntimeError::Coordination("workspace integration does not exist".into())
            })?;
        super::validate_coordination_actor(
            actor,
            &integration.approval.session_id,
            &integration.approval.approved_by,
        )?;
        coordinator
            .apply_integration(integration_id, crate::session::now_secs())
            .await
            .map_err(workspace_error)
    }

    fn workspace_coordinator(
        &self,
    ) -> Result<&crate::workspace::agent_views::AgentWorkspaceCoordinator, RuntimeError> {
        self.agent_workspaces.as_deref().ok_or_else(|| {
            RuntimeError::Coordination("Agent workspace coordination is unavailable".into())
        })
    }

    async fn authorize_workspace_reviewer(
        &self,
        actor: &AgentInstanceId,
        view: &crate::coordination::workspace::AgentWorkspaceView,
    ) -> Result<(), RuntimeError> {
        let membership = self
            .storage
            .sessions()
            .session_membership(&view.session_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .ok_or_else(|| RuntimeError::Coordination("workspace membership disappeared".into()))?;
        if actor == &view.agent_instance_id || actor == &membership.governance.moderator_instance_id
        {
            return Ok(());
        }
        let topology = self
            .coordination_service()?
            .topology(&view.session_id)
            .await
            .map_err(super::coordination_error)?;
        if topology.relations.iter().any(|relation| {
            relation.kind == AgentRelationKind::Reviews
                && &relation.source == actor
                && relation.target == view.agent_instance_id
        }) {
            Ok(())
        } else {
            Err(RuntimeError::Coordination(
                "Agent is not authorized to review this workspace".into(),
            ))
        }
    }
}

fn workspace_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Coordination(error.to_string())
}
