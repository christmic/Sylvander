//! `SQLite` persistence for fenced Agent workspace views.

use std::path::PathBuf;

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use sylvander_api::{AgentInstanceId, SessionId, WorkspaceIntegrationId, WorkspaceViewId};

use crate::coordination::workspace::{
    AgentWorkspaceView, WorkspaceAccess, WorkspaceIntegration, WorkspaceIntegrationApproval,
    WorkspaceIntegrationState, WorkspaceIsolation, WorkspaceViewState,
};
use crate::session::membership::SessionMembership;
use crate::storage::session::{SessionStoreError, SqliteSessionStore};

#[async_trait]
pub trait AgentWorkspaceStore: Send + Sync {
    async fn create_workspace_view(
        &self,
        view: &AgentWorkspaceView,
        membership: &SessionMembership,
    ) -> Result<(), SessionStoreError>;

    async fn workspace_view(
        &self,
        view_id: &WorkspaceViewId,
    ) -> Result<Option<AgentWorkspaceView>, SessionStoreError>;

    async fn active_workspace_views(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<AgentWorkspaceView>, SessionStoreError>;

    async fn transition_workspace_view(
        &self,
        view_id: &WorkspaceViewId,
        expected_revision: u64,
        lease_epoch: u64,
        fencing_token: u64,
        next: WorkspaceViewState,
        now: i64,
    ) -> Result<AgentWorkspaceView, SessionStoreError>;

    async fn create_workspace_integration(
        &self,
        integration: &WorkspaceIntegration,
        view: &AgentWorkspaceView,
        membership: &SessionMembership,
        topology_revision: u64,
    ) -> Result<(), SessionStoreError>;

    async fn workspace_integration(
        &self,
        integration_id: &WorkspaceIntegrationId,
    ) -> Result<Option<WorkspaceIntegration>, SessionStoreError>;

    async fn transition_workspace_integration(
        &self,
        integration_id: &WorkspaceIntegrationId,
        expected_revision: u64,
        next: WorkspaceIntegrationState,
        now: i64,
    ) -> Result<WorkspaceIntegration, SessionStoreError>;
}

#[async_trait]
impl AgentWorkspaceStore for SqliteSessionStore {
    async fn create_workspace_view(
        &self,
        view: &AgentWorkspaceView,
        membership: &SessionMembership,
    ) -> Result<(), SessionStoreError> {
        view.validate_new(membership)
            .map_err(|error| SessionStoreError::Invalid(error.to_string()))?;
        let view = view.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let durable_revision = transaction
                .query_row(
                    "SELECT membership_revision FROM session_governance WHERE session_id=?1",
                    [&view.session_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .map(|value| checked_u64(value, "membership revision"))
                .transpose()?;
            if durable_revision != Some(view.membership_revision) {
                return Err(SessionStoreError::Invalid(
                    "workspace view membership changed before commit".into(),
                ));
            }
            if load_view(&transaction, &view.view_id)?.is_some() {
                return Err(SessionStoreError::Invalid(
                    "workspace view already exists".into(),
                ));
            }
            let has_active_view = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_workspace_views \
                 WHERE session_id=?1 AND agent_instance_id=?2 \
                 AND state IN ('provisioning','active','integrating','conflicted','manual_reconciliation'))",
                params![view.session_id.0, view.agent_instance_id.0],
                |row| row.get::<_, bool>(0),
            )?;
            if has_active_view {
                return Err(SessionStoreError::Invalid(
                    "Agent already owns an active workspace view".into(),
                ));
            }
            let source = view
                .source_workspace
                .to_str()
                .ok_or_else(|| SessionStoreError::Invalid("workspace path is not UTF-8".into()))?;
            let effective = view.effective_workspace.to_str().ok_or_else(|| {
                SessionStoreError::Invalid("effective workspace path is not UTF-8".into())
            })?;
            transaction.execute(
                    "INSERT INTO agent_workspace_views \
                     (view_id,session_id,agent_instance_id,membership_revision,access_kind,
                      isolation_kind,source_workspace,effective_workspace,target_id,branch,
                      base_revision,state,lease_epoch,fencing_token,revision,created_at,updated_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'provisioning',?12,?13,0,?14,?15)",
                    params![
                        view.view_id.0,
                        view.session_id.0,
                        view.agent_instance_id.0,
                        checked_i64(view.membership_revision, "membership revision")?,
                        encode_access(view.access),
                        encode_isolation(view.isolation),
                        source,
                        effective,
                        view.target_id,
                        view.branch,
                        view.base_revision,
                        checked_i64(view.lease_epoch, "workspace lease epoch")?,
                        checked_i64(view.fencing_token, "workspace fencing token")?,
                        view.created_at,
                        view.updated_at,
                    ],
                )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    async fn workspace_view(
        &self,
        view_id: &WorkspaceViewId,
    ) -> Result<Option<AgentWorkspaceView>, SessionStoreError> {
        let view_id = view_id.clone();
        self.run(move |connection| load_view(connection, &view_id))
            .await
    }

    async fn active_workspace_views(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<AgentWorkspaceView>, SessionStoreError> {
        let session_id = session_id.clone();
        self.run(move |connection| {
            let ids = {
                let mut statement = connection.prepare(
                    "SELECT view_id FROM agent_workspace_views WHERE session_id=?1 \
                     AND state IN ('provisioning','active','integrating','conflicted','manual_reconciliation') \
                     ORDER BY created_at,view_id",
                )?;
                statement
                    .query_map([&session_id.0], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            ids.into_iter()
                .map(|id| {
                    load_view(connection, &WorkspaceViewId::new(id))?.ok_or_else(|| {
                        SessionStoreError::Store(
                            "workspace view disappeared while listing".into(),
                        )
                    })
                })
                .collect()
        })
        .await
    }

    async fn transition_workspace_view(
        &self,
        view_id: &WorkspaceViewId,
        expected_revision: u64,
        lease_epoch: u64,
        fencing_token: u64,
        next: WorkspaceViewState,
        now: i64,
    ) -> Result<AgentWorkspaceView, SessionStoreError> {
        let view_id = view_id.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let Some(mut view) = load_view(&transaction, &view_id)? else {
                return Err(SessionStoreError::Invalid(
                    "workspace view does not exist".into(),
                ));
            };
            view.transition(expected_revision, lease_epoch, fencing_token, next, now)
                .map_err(|error| SessionStoreError::Invalid(error.to_string()))?;
            let changed = transaction.execute(
                "UPDATE agent_workspace_views SET state=?1,revision=?2,updated_at=?3 \
                 WHERE view_id=?4 AND revision=?5 AND lease_epoch=?6 AND fencing_token=?7",
                params![
                    encode_state(view.state),
                    checked_i64(view.revision, "workspace view revision")?,
                    view.updated_at,
                    view.view_id.0,
                    checked_i64(expected_revision, "expected workspace view revision")?,
                    checked_i64(lease_epoch, "workspace lease epoch")?,
                    checked_i64(fencing_token, "workspace fencing token")?,
                ],
            )?;
            if changed != 1 {
                return Err(SessionStoreError::Invalid(
                    "workspace view changed before transition commit".into(),
                ));
            }
            transaction.commit()?;
            Ok(view)
        })
        .await
    }

    async fn create_workspace_integration(
        &self,
        integration: &WorkspaceIntegration,
        view: &AgentWorkspaceView,
        membership: &SessionMembership,
        topology_revision: u64,
    ) -> Result<(), SessionStoreError> {
        let expected = WorkspaceIntegration::new(
            integration.approval.clone(),
            view,
            membership,
            topology_revision,
        )
        .map_err(|error| SessionStoreError::Invalid(error.to_string()))?;
        if integration != &expected {
            return Err(SessionStoreError::Invalid(
                "workspace integration is not a new approved record".into(),
            ));
        }
        let integration = integration.clone();
        let view = view.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let durable_view = load_view(&transaction, &view.view_id)?.ok_or_else(|| {
                SessionStoreError::Invalid("workspace integration view does not exist".into())
            })?;
            if durable_view != view {
                return Err(SessionStoreError::Invalid(
                    "workspace view changed before integration approval commit".into(),
                ));
            }
            let topology = transaction
                .query_row(
                    "SELECT topology_revision FROM session_topology WHERE session_id=?1",
                    [&view.session_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .map(|value| checked_u64(value, "topology revision"))
                .transpose()?;
            if topology != Some(integration.approval.topology_revision) {
                return Err(SessionStoreError::Invalid(
                    "workspace integration topology changed before commit".into(),
                ));
            }
            if load_integration(&transaction, &integration.approval.integration_id)?.is_some() {
                return Err(SessionStoreError::Invalid(
                    "workspace integration already exists".into(),
                ));
            }
            let has_active = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM workspace_integrations WHERE view_id=?1 \
                 AND state IN ('approved','applying','conflicted','manual_reconciliation'))",
                [&view.view_id.0],
                |row| row.get::<_, bool>(0),
            )?;
            if has_active {
                return Err(SessionStoreError::Invalid(
                    "workspace view already has an active integration".into(),
                ));
            }
            let approval = &integration.approval;
            transaction.execute(
                "INSERT INTO workspace_integrations \
                 (integration_id,view_id,session_id,agent_instance_id,approved_by_instance_id,
                  membership_revision,topology_revision,view_revision,lease_epoch,fencing_token,
                  review_digest,approved_at,state,revision,updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'approved',0,?13)",
                params![
                    approval.integration_id.0,
                    approval.view_id.0,
                    approval.session_id.0,
                    approval.agent_instance_id.0,
                    approval.approved_by.0,
                    checked_i64(approval.membership_revision, "membership revision")?,
                    checked_i64(approval.topology_revision, "topology revision")?,
                    checked_i64(approval.view_revision, "workspace view revision")?,
                    checked_i64(approval.lease_epoch, "workspace lease epoch")?,
                    checked_i64(approval.fencing_token, "workspace fencing token")?,
                    approval.review_digest,
                    approval.approved_at,
                    integration.updated_at,
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    async fn workspace_integration(
        &self,
        integration_id: &WorkspaceIntegrationId,
    ) -> Result<Option<WorkspaceIntegration>, SessionStoreError> {
        let integration_id = integration_id.clone();
        self.run(move |connection| load_integration(connection, &integration_id))
            .await
    }

    async fn transition_workspace_integration(
        &self,
        integration_id: &WorkspaceIntegrationId,
        expected_revision: u64,
        next: WorkspaceIntegrationState,
        now: i64,
    ) -> Result<WorkspaceIntegration, SessionStoreError> {
        let integration_id = integration_id.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let Some(mut integration) = load_integration(&transaction, &integration_id)? else {
                return Err(SessionStoreError::Invalid(
                    "workspace integration does not exist".into(),
                ));
            };
            integration
                .transition(expected_revision, next, now)
                .map_err(|error| SessionStoreError::Invalid(error.to_string()))?;
            let changed = transaction.execute(
                "UPDATE workspace_integrations SET state=?1,revision=?2,updated_at=?3 \
                 WHERE integration_id=?4 AND revision=?5",
                params![
                    encode_integration_state(integration.state),
                    checked_i64(integration.revision, "workspace integration revision")?,
                    integration.updated_at,
                    integration.approval.integration_id.0,
                    checked_i64(expected_revision, "expected integration revision")?,
                ],
            )?;
            if changed != 1 {
                return Err(SessionStoreError::Invalid(
                    "workspace integration changed before transition commit".into(),
                ));
            }
            transaction.commit()?;
            Ok(integration)
        })
        .await
    }
}

fn load_integration(
    connection: &rusqlite::Connection,
    integration_id: &WorkspaceIntegrationId,
) -> Result<Option<WorkspaceIntegration>, SessionStoreError> {
    connection
        .query_row(
            "SELECT view_id,session_id,agent_instance_id,approved_by_instance_id,
                    membership_revision,topology_revision,view_revision,lease_epoch,fencing_token,
                    review_digest,approved_at,state,revision,updated_at
             FROM workspace_integrations WHERE integration_id=?1",
            [&integration_id.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                ))
            },
        )
        .optional()?
        .map(|row| {
            Ok(WorkspaceIntegration {
                approval: WorkspaceIntegrationApproval {
                    integration_id: integration_id.clone(),
                    view_id: WorkspaceViewId::new(row.0),
                    session_id: SessionId::new(row.1),
                    agent_instance_id: AgentInstanceId::new(row.2),
                    approved_by: AgentInstanceId::new(row.3),
                    membership_revision: checked_u64(row.4, "membership revision")?,
                    topology_revision: checked_u64(row.5, "topology revision")?,
                    view_revision: checked_u64(row.6, "workspace view revision")?,
                    lease_epoch: checked_u64(row.7, "workspace lease epoch")?,
                    fencing_token: checked_u64(row.8, "workspace fencing token")?,
                    review_digest: row.9,
                    approved_at: row.10,
                },
                state: decode_integration_state(&row.11)?,
                revision: checked_u64(row.12, "workspace integration revision")?,
                updated_at: row.13,
            })
        })
        .transpose()
}

fn load_view(
    connection: &rusqlite::Connection,
    view_id: &WorkspaceViewId,
) -> Result<Option<AgentWorkspaceView>, SessionStoreError> {
    connection
        .query_row(
            "SELECT session_id,agent_instance_id,membership_revision,access_kind,isolation_kind,
                    source_workspace,effective_workspace,target_id,branch,base_revision,state,
                    lease_epoch,fencing_token,revision,created_at,updated_at
             FROM agent_workspace_views WHERE view_id=?1",
            [&view_id.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                ))
            },
        )
        .optional()?
        .map(|row| {
            Ok(AgentWorkspaceView {
                view_id: view_id.clone(),
                session_id: SessionId::new(row.0),
                agent_instance_id: AgentInstanceId::new(row.1),
                membership_revision: checked_u64(row.2, "membership revision")?,
                access: decode_access(&row.3)?,
                isolation: decode_isolation(&row.4)?,
                source_workspace: PathBuf::from(row.5),
                effective_workspace: PathBuf::from(row.6),
                target_id: row.7,
                branch: row.8,
                base_revision: row.9,
                state: decode_state(&row.10)?,
                lease_epoch: checked_u64(row.11, "workspace lease epoch")?,
                fencing_token: checked_u64(row.12, "workspace fencing token")?,
                revision: checked_u64(row.13, "workspace view revision")?,
                created_at: row.14,
                updated_at: row.15,
            })
        })
        .transpose()
}

const fn encode_access(access: WorkspaceAccess) -> &'static str {
    match access {
        WorkspaceAccess::ReadOnly => "read_only",
        WorkspaceAccess::ReadWrite => "read_write",
    }
}

fn decode_access(value: &str) -> Result<WorkspaceAccess, SessionStoreError> {
    match value {
        "read_only" => Ok(WorkspaceAccess::ReadOnly),
        "read_write" => Ok(WorkspaceAccess::ReadWrite),
        _ => Err(SessionStoreError::Store(
            "stored workspace access is invalid".into(),
        )),
    }
}

const fn encode_isolation(isolation: WorkspaceIsolation) -> &'static str {
    match isolation {
        WorkspaceIsolation::Shared => "shared",
        WorkspaceIsolation::IsolatedWorktree => "isolated_worktree",
    }
}

fn decode_isolation(value: &str) -> Result<WorkspaceIsolation, SessionStoreError> {
    match value {
        "shared" => Ok(WorkspaceIsolation::Shared),
        "isolated_worktree" => Ok(WorkspaceIsolation::IsolatedWorktree),
        _ => Err(SessionStoreError::Store(
            "stored workspace isolation is invalid".into(),
        )),
    }
}

const fn encode_state(state: WorkspaceViewState) -> &'static str {
    match state {
        WorkspaceViewState::Provisioning => "provisioning",
        WorkspaceViewState::Active => "active",
        WorkspaceViewState::Integrating => "integrating",
        WorkspaceViewState::Integrated => "integrated",
        WorkspaceViewState::Conflicted => "conflicted",
        WorkspaceViewState::Released => "released",
        WorkspaceViewState::ManualReconciliation => "manual_reconciliation",
    }
}

fn decode_state(value: &str) -> Result<WorkspaceViewState, SessionStoreError> {
    match value {
        "provisioning" => Ok(WorkspaceViewState::Provisioning),
        "active" => Ok(WorkspaceViewState::Active),
        "integrating" => Ok(WorkspaceViewState::Integrating),
        "integrated" => Ok(WorkspaceViewState::Integrated),
        "conflicted" => Ok(WorkspaceViewState::Conflicted),
        "released" => Ok(WorkspaceViewState::Released),
        "manual_reconciliation" => Ok(WorkspaceViewState::ManualReconciliation),
        _ => Err(SessionStoreError::Store(
            "stored workspace state is invalid".into(),
        )),
    }
}

const fn encode_integration_state(state: WorkspaceIntegrationState) -> &'static str {
    match state {
        WorkspaceIntegrationState::Approved => "approved",
        WorkspaceIntegrationState::Applying => "applying",
        WorkspaceIntegrationState::Applied => "applied",
        WorkspaceIntegrationState::Conflicted => "conflicted",
        WorkspaceIntegrationState::ManualReconciliation => "manual_reconciliation",
    }
}

fn decode_integration_state(value: &str) -> Result<WorkspaceIntegrationState, SessionStoreError> {
    match value {
        "approved" => Ok(WorkspaceIntegrationState::Approved),
        "applying" => Ok(WorkspaceIntegrationState::Applying),
        "applied" => Ok(WorkspaceIntegrationState::Applied),
        "conflicted" => Ok(WorkspaceIntegrationState::Conflicted),
        "manual_reconciliation" => Ok(WorkspaceIntegrationState::ManualReconciliation),
        _ => Err(SessionStoreError::Store(
            "stored workspace integration state is invalid".into(),
        )),
    }
}

fn checked_i64(value: u64, label: &str) -> Result<i64, SessionStoreError> {
    value
        .try_into()
        .map_err(|_| SessionStoreError::Invalid(format!("{label} exceeds SQLite range")))
}

fn checked_u64(value: i64, label: &str) -> Result<u64, SessionStoreError> {
    value
        .try_into()
        .map_err(|_| SessionStoreError::Store(format!("stored {label} is negative")))
}

#[cfg(test)]
#[path = "../../tests/unit/workspace_coordination_store.rs"]
mod tests;
