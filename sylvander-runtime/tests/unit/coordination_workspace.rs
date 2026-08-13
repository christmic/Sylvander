use crate::agent::instance::{
    AgentDefinitionKey, AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute,
    HistoryView, SessionAgentRole,
};
use crate::session::membership::SessionGovernance;
use sylvander_api::AgentId;

use super::*;

fn membership() -> SessionMembership {
    let session_id = SessionId::new("session");
    let instance = |id: &str, role| AgentInstance {
        instance_id: AgentInstanceId::new(id),
        session_id: session_id.clone(),
        definition: AgentDefinitionKey {
            agent_id: AgentId::new(id),
            revision: 1,
        },
        origin: AgentInstanceOrigin::Defined,
        role,
        history_view: HistoryView::SharedLane { cursor: 0 },
        approval_route: ApprovalRoute::User,
        state: AgentInstanceState::Ready,
        lifecycle_revision: 0,
        capability_revision: "sha256:capabilities".into(),
        created_at: 1,
        updated_at: 1,
    };
    SessionMembership::new(
        session_id.clone(),
        vec![
            instance("moderator", SessionAgentRole::Moderator),
            instance("worker", SessionAgentRole::Worker),
        ],
        SessionGovernance {
            session_id,
            moderator_instance_id: AgentInstanceId::new("moderator"),
            governance_revision: "sha256:governance".into(),
            membership_revision: 2,
            lease_epoch: 3,
            fencing_token: 4,
            updated_at: 1,
        },
    )
    .unwrap()
}

fn isolated_view() -> AgentWorkspaceView {
    AgentWorkspaceView {
        view_id: WorkspaceViewId::new("view"),
        session_id: SessionId::new("session"),
        agent_instance_id: AgentInstanceId::new("worker"),
        membership_revision: 2,
        access: WorkspaceAccess::ReadWrite,
        isolation: WorkspaceIsolation::IsolatedWorktree,
        source_workspace: PathBuf::from("/repo"),
        effective_workspace: PathBuf::from("/leases/view"),
        target_id: Some("local".into()),
        branch: Some("sylvander/view".into()),
        base_revision: Some("abc123".into()),
        state: WorkspaceViewState::Provisioning,
        lease_epoch: 5,
        fencing_token: 9,
        revision: 0,
        created_at: 1,
        updated_at: 1,
    }
}

#[test]
fn writable_shared_workspace_is_rejected() {
    let mut view = isolated_view();
    view.isolation = WorkspaceIsolation::Shared;
    view.effective_workspace.clone_from(&view.source_workspace);
    view.branch = None;
    view.base_revision = None;

    assert_eq!(
        view.validate_new(&membership()),
        Err(WorkspaceViewError::SharedWriteForbidden)
    );
}

#[test]
fn workspace_lifecycle_requires_exact_revision_and_fencing() {
    let mut view = isolated_view();
    view.validate_new(&membership()).unwrap();

    assert_eq!(
        view.transition(0, 5, 8, WorkspaceViewState::Active, 2),
        Err(WorkspaceViewError::StaleLease)
    );
    view.transition(0, 5, 9, WorkspaceViewState::Active, 2)
        .unwrap();
    assert_eq!(view.state, WorkspaceViewState::Active);
    assert_eq!(view.revision, 1);
    assert_eq!(
        view.transition(0, 5, 9, WorkspaceViewState::Integrating, 3),
        Err(WorkspaceViewError::RevisionConflict)
    );
}

#[test]
fn only_moderator_can_approve_an_exact_reviewed_workspace_revision() {
    let mut view = isolated_view();
    view.state = WorkspaceViewState::Active;
    view.revision = 1;
    let mut approval = WorkspaceIntegrationApproval {
        integration_id: WorkspaceIntegrationId::new("integration"),
        view_id: view.view_id.clone(),
        session_id: view.session_id.clone(),
        agent_instance_id: view.agent_instance_id.clone(),
        approved_by: AgentInstanceId::new("moderator"),
        membership_revision: 2,
        topology_revision: 7,
        view_revision: 1,
        lease_epoch: 5,
        fencing_token: 9,
        review_digest: "sha256:reviewed-candidate".into(),
        approved_at: 3,
    };

    approval.validate(&view, &membership(), 7).unwrap();
    let mut integration =
        WorkspaceIntegration::new(approval.clone(), &view, &membership(), 7).unwrap();
    integration
        .transition(0, WorkspaceIntegrationState::Applying, 4)
        .unwrap();
    integration
        .transition(1, WorkspaceIntegrationState::Applied, 5)
        .unwrap();
    assert_eq!(integration.revision, 2);
    approval.approved_by = AgentInstanceId::new("worker");
    assert_eq!(
        approval.validate(&view, &membership(), 7),
        Err(WorkspaceViewError::UnauthorizedIntegrator)
    );
}
