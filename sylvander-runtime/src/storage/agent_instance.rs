//! Durable repository for Session membership of concrete Agent instances.

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use sylvander_api::{AgentId, AgentInstanceId, SessionId, SwarmId};

use crate::agent::instance::{
    AgentDefinitionKey, AgentInstance, AgentInstanceState, SessionAgentRole,
};
use crate::coordination::topology::{SessionTopology, encode_relation_kind};
use crate::session::membership::{SessionGovernance, SessionMembership};
use crate::storage::session::{SessionStoreError, SqliteSessionStore};

/// Runtime-owned persistence port for Agent participants and moderator truth.
#[async_trait]
pub trait AgentInstanceStore: Send + Sync {
    /// Atomically initialize or compare-and-set the complete membership.
    async fn save_session_membership(
        &self,
        membership: &SessionMembership,
        expected_revision: Option<u64>,
    ) -> Result<(), SessionStoreError>;

    /// Load the complete membership or `None` when it has not been initialized.
    async fn session_membership(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionMembership>, SessionStoreError>;

    /// Atomically append one participant and replace topology at the next revisions.
    async fn add_session_participant(
        &self,
        participant: &AgentInstance,
        next_membership: &SessionMembership,
        next_topology: &SessionTopology,
        expected_membership_revision: u64,
        expected_topology_revision: u64,
    ) -> Result<(), SessionStoreError>;
}

#[async_trait]
impl AgentInstanceStore for SqliteSessionStore {
    async fn save_session_membership(
        &self,
        membership: &SessionMembership,
        expected_revision: Option<u64>,
    ) -> Result<(), SessionStoreError> {
        membership
            .validate()
            .map_err(|error| SessionStoreError::Invalid(error.to_string()))?;
        let membership = membership.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let exists = transaction
                .query_row(
                    "SELECT 1 FROM sessions WHERE id=?1 AND is_archived=0",
                    [&membership.session_id.0],
                    |_| Ok(()),
                )
                .optional()?;
            if exists.is_none() {
                return Err(SessionStoreError::NotFound(membership.session_id));
            }
            let actual_revision = transaction
                .query_row(
                    "SELECT membership_revision FROM session_governance WHERE session_id=?1",
                    [&membership.session_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .map(|value| checked_u64(value, "membership revision"))
                .transpose()?;
            if actual_revision != expected_revision {
                return Err(SessionStoreError::MembershipConflict {
                    expected: expected_revision,
                    actual: actual_revision,
                });
            }
            let required_revision = match expected_revision {
                None => 0,
                Some(revision) => revision.checked_add(1).ok_or_else(|| {
                    SessionStoreError::Invalid("membership revision overflow".into())
                })?,
            };
            if membership.governance.membership_revision != required_revision {
                return Err(SessionStoreError::Invalid(
                    "next membership revision is not sequential".into(),
                ));
            }

            transaction.execute(
                "DELETE FROM session_governance WHERE session_id=?1",
                [&membership.session_id.0],
            )?;
            transaction.execute(
                "DELETE FROM session_topology WHERE session_id=?1",
                [&membership.session_id.0],
            )?;
            transaction.execute(
                "DELETE FROM session_agent_instances WHERE session_id=?1",
                [&membership.session_id.0],
            )?;
            transaction.execute(
                "DELETE FROM session_agents WHERE session_id=?1",
                [&membership.session_id.0],
            )?;

            let mut definitions = std::collections::HashSet::new();
            for (ordinal, participant) in membership.participants.iter().enumerate() {
                insert_agent_instance(&transaction, participant, ordinal)?;
                definitions.insert(participant.definition.agent_id.clone());
            }
            for definition in definitions {
                transaction.execute(
                    "INSERT INTO session_agents(session_id,agent_id,joined_at) VALUES (?1,?2,?3)",
                    params![
                        membership.session_id.0,
                        definition.0,
                        membership.governance.updated_at
                    ],
                )?;
            }
            transaction.execute(
                "INSERT INTO session_governance \
                 (session_id,moderator_instance_id,moderator_role,governance_revision,\
                  membership_revision,lease_epoch,fencing_token,updated_at) \
                 VALUES (?1,?2,'moderator',?3,?4,?5,?6,?7)",
                params![
                    membership.session_id.0,
                    membership.governance.moderator_instance_id.0,
                    membership.governance.governance_revision,
                    checked_i64(
                        membership.governance.membership_revision,
                        "membership revision"
                    )?,
                    checked_i64(membership.governance.lease_epoch, "moderator lease epoch")?,
                    checked_i64(
                        membership.governance.fencing_token,
                        "moderator fencing token"
                    )?,
                    membership.governance.updated_at,
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    async fn session_membership(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionMembership>, SessionStoreError> {
        let session_id = session_id.clone();
        self.run(move |connection| {
            let governance = connection
                .query_row(
                    "SELECT moderator_instance_id,governance_revision,membership_revision,\
                            lease_epoch,fencing_token,updated_at \
                     FROM session_governance WHERE session_id=?1",
                    [&session_id.0],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                        ))
                    },
                )
                .optional()?;
            let Some((moderator, revision, membership_revision, epoch, fencing, updated_at)) =
                governance
            else {
                return Ok(None);
            };

            let mut statement = connection.prepare(
                "SELECT instance_id,agent_id,definition_revision,origin_json,role,role_swarm_id,\
                        history_view_json,approval_route_json,state,capability_revision,\
                        lifecycle_revision,created_at,updated_at \
                 FROM session_agent_instances WHERE session_id=?1 ORDER BY membership_ordinal",
            )?;
            let rows = statement.query_map([&session_id.0], |row| {
                Ok(EncodedInstance {
                    instance_id: row.get(0)?,
                    agent_id: row.get(1)?,
                    definition_revision: row.get(2)?,
                    origin: row.get(3)?,
                    role: row.get(4)?,
                    role_swarm_id: row.get(5)?,
                    history_view: row.get(6)?,
                    approval_route: row.get(7)?,
                    state: row.get(8)?,
                    capability_revision: row.get(9)?,
                    lifecycle_revision: row.get(10)?,
                    created_at: row.get(11)?,
                    updated_at: row.get(12)?,
                })
            })?;
            let participants = rows
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|row| decode_instance(&session_id, row))
                .collect::<Result<Vec<_>, _>>()?;
            let governance = SessionGovernance {
                session_id: session_id.clone(),
                moderator_instance_id: AgentInstanceId::new(moderator),
                governance_revision: revision,
                membership_revision: checked_u64(membership_revision, "membership revision")?,
                lease_epoch: checked_u64(epoch, "moderator lease epoch")?,
                fencing_token: checked_u64(fencing, "moderator fencing token")?,
                updated_at,
            };
            SessionMembership::new(session_id, participants, governance)
                .map(Some)
                .map_err(|error| SessionStoreError::Store(error.to_string()))
        })
        .await
    }

    async fn add_session_participant(
        &self,
        participant: &AgentInstance,
        next_membership: &SessionMembership,
        next_topology: &SessionTopology,
        expected_membership_revision: u64,
        expected_topology_revision: u64,
    ) -> Result<(), SessionStoreError> {
        next_membership
            .validate()
            .map_err(|error| SessionStoreError::Invalid(error.to_string()))?;
        next_topology
            .validate(next_membership)
            .map_err(|error| SessionStoreError::Invalid(error.to_string()))?;
        let required_membership_revision = expected_membership_revision
            .checked_add(1)
            .ok_or_else(|| SessionStoreError::Invalid("membership revision overflow".into()))?;
        let required_topology_revision = expected_topology_revision
            .checked_add(1)
            .ok_or_else(|| SessionStoreError::Invalid("topology revision overflow".into()))?;
        if participant.session_id != next_membership.session_id
            || next_membership.participants.last() != Some(participant)
            || next_membership.governance.membership_revision != required_membership_revision
            || next_topology.membership_revision != next_membership.governance.membership_revision
            || next_topology.topology_revision != required_topology_revision
        {
            return Err(SessionStoreError::Invalid(
                "participant append does not contain sequential exact facts".into(),
            ));
        }
        let participant = participant.clone();
        let membership = next_membership.clone();
        let topology = next_topology.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let current = transaction
                .query_row(
                    "SELECT moderator_instance_id,governance_revision,membership_revision,
                            lease_epoch,fencing_token FROM session_governance WHERE session_id=?1",
                    [&membership.session_id.0],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some(current) = current else {
                return Err(SessionStoreError::MembershipConflict {
                    expected: Some(expected_membership_revision),
                    actual: None,
                });
            };
            let actual_membership = checked_u64(current.2, "membership revision")?;
            let actual_topology = transaction
                .query_row(
                    "SELECT topology_revision FROM session_topology WHERE session_id=?1",
                    [&membership.session_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .map(|value| checked_u64(value, "topology revision"))
                .transpose()?;
            if actual_membership != expected_membership_revision {
                return Err(SessionStoreError::MembershipConflict {
                    expected: Some(expected_membership_revision),
                    actual: Some(actual_membership),
                });
            }
            if actual_topology != Some(expected_topology_revision) {
                return Err(SessionStoreError::TopologyConflict {
                    expected: Some(expected_topology_revision),
                    actual: actual_topology,
                });
            }
            if current.0 != membership.governance.moderator_instance_id.0
                || current.1 != membership.governance.governance_revision
                || checked_u64(current.3, "moderator lease epoch")?
                    != membership.governance.lease_epoch
                || checked_u64(current.4, "moderator fencing token")?
                    != membership.governance.fencing_token
            {
                return Err(SessionStoreError::Invalid(
                    "participant append cannot change moderator governance".into(),
                ));
            }
            let existing_count = transaction.query_row(
                "SELECT COUNT(*) FROM session_agent_instances WHERE session_id=?1",
                [&membership.session_id.0],
                |row| row.get::<_, i64>(0),
            )?;
            if usize::try_from(existing_count).ok().and_then(|count| count.checked_add(1))
                != Some(membership.participants.len())
            {
                return Err(SessionStoreError::Invalid(
                    "participant append must add exactly one Agent".into(),
                ));
            }
            insert_agent_instance(
                &transaction,
                &participant,
                membership.participants.len().saturating_sub(1),
            )?;
            transaction.execute(
                "INSERT OR IGNORE INTO session_agents(session_id,agent_id,joined_at) VALUES (?1,?2,?3)",
                params![membership.session_id.0, participant.definition.agent_id.0, participant.created_at],
            )?;
            transaction.execute(
                "UPDATE session_governance SET membership_revision=?1,updated_at=?2 \
                 WHERE session_id=?3 AND membership_revision=?4",
                params![
                    checked_i64(membership.governance.membership_revision, "membership revision")?,
                    membership.governance.updated_at,
                    membership.session_id.0,
                    checked_i64(expected_membership_revision, "membership revision")?,
                ],
            )?;
            transaction.execute(
                "UPDATE coordination_tasks SET membership_revision=?1 \
                 WHERE session_id=?2 AND membership_revision=?3",
                params![
                    checked_i64(membership.governance.membership_revision, "membership revision")?,
                    membership.session_id.0,
                    checked_i64(expected_membership_revision, "membership revision")?,
                ],
            )?;
            transaction.execute(
                "DELETE FROM session_topology WHERE session_id=?1",
                [&topology.session_id.0],
            )?;
            transaction.execute(
                "INSERT INTO session_topology(session_id,membership_revision,topology_revision,updated_at) \
                 VALUES (?1,?2,?3,?4)",
                params![
                    topology.session_id.0,
                    checked_i64(topology.membership_revision, "membership revision")?,
                    checked_i64(topology.topology_revision, "topology revision")?,
                    topology.updated_at,
                ],
            )?;
            for (ordinal, relation) in topology.relations.iter().enumerate() {
                transaction.execute(
                    "INSERT INTO agent_relations(session_id,relation_ordinal,source_instance_id,
                     target_instance_id,relation_kind,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        topology.session_id.0,
                        i64::try_from(ordinal).map_err(|_| SessionStoreError::Invalid(
                            "Agent relation ordinal exceeds SQLite range".into()
                        ))?,
                        relation.source.0,
                        relation.target.0,
                        encode_relation_kind(relation.kind),
                        relation.created_at,
                    ],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
    }
}

fn insert_agent_instance(
    transaction: &rusqlite::Transaction<'_>,
    participant: &AgentInstance,
    ordinal: usize,
) -> Result<(), SessionStoreError> {
    let (role, swarm_id) = encode_role(&participant.role);
    transaction.execute(
        "INSERT INTO session_agent_instances \
         (instance_id,session_id,membership_ordinal,agent_id,definition_revision,origin_json,role,
          role_swarm_id,history_view_json,approval_route_json,state,capability_revision,
          lifecycle_revision,created_at,updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            participant.instance_id.0,
            participant.session_id.0,
            i64::try_from(ordinal).map_err(|_| SessionStoreError::Invalid(
                "Agent membership ordinal exceeds SQLite range".into()
            ))?,
            participant.definition.agent_id.0,
            checked_i64(participant.definition.revision, "Agent definition revision")?,
            encode_json(&participant.origin)?,
            role,
            swarm_id,
            encode_json(&participant.history_view)?,
            encode_json(&participant.approval_route)?,
            encode_state(participant.state),
            participant.capability_revision,
            checked_i64(participant.lifecycle_revision, "Agent lifecycle revision")?,
            participant.created_at,
            participant.updated_at,
        ],
    )?;
    Ok(())
}

struct EncodedInstance {
    instance_id: String,
    agent_id: String,
    definition_revision: i64,
    origin: String,
    role: String,
    role_swarm_id: Option<String>,
    history_view: String,
    approval_route: String,
    state: String,
    capability_revision: String,
    lifecycle_revision: i64,
    created_at: i64,
    updated_at: i64,
}

fn decode_instance(
    session_id: &SessionId,
    row: EncodedInstance,
) -> Result<AgentInstance, SessionStoreError> {
    Ok(AgentInstance {
        instance_id: AgentInstanceId::new(row.instance_id),
        session_id: session_id.clone(),
        definition: AgentDefinitionKey {
            agent_id: AgentId::new(row.agent_id),
            revision: checked_u64(row.definition_revision, "Agent definition revision")?,
        },
        origin: decode_json(&row.origin, "Agent instance origin")?,
        role: decode_role(&row.role, row.role_swarm_id)?,
        history_view: decode_json(&row.history_view, "Agent history view")?,
        approval_route: decode_json(&row.approval_route, "Agent approval route")?,
        state: decode_state(&row.state)?,
        lifecycle_revision: checked_u64(row.lifecycle_revision, "Agent lifecycle revision")?,
        capability_revision: row.capability_revision,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn encode_role(role: &SessionAgentRole) -> (&'static str, Option<&str>) {
    match role {
        SessionAgentRole::Moderator => ("moderator", None),
        SessionAgentRole::Coordinator { swarm_id } => ("coordinator", Some(&swarm_id.0)),
        SessionAgentRole::Worker => ("worker", None),
        SessionAgentRole::Reviewer => ("reviewer", None),
        SessionAgentRole::Specialist => ("specialist", None),
        SessionAgentRole::Observer => ("observer", None),
    }
}

fn decode_role(
    role: &str,
    swarm_id: Option<String>,
) -> Result<SessionAgentRole, SessionStoreError> {
    match (role, swarm_id) {
        ("moderator", None) => Ok(SessionAgentRole::Moderator),
        ("coordinator", Some(id)) => Ok(SessionAgentRole::Coordinator {
            swarm_id: SwarmId::new(id),
        }),
        ("worker", None) => Ok(SessionAgentRole::Worker),
        ("reviewer", None) => Ok(SessionAgentRole::Reviewer),
        ("specialist", None) => Ok(SessionAgentRole::Specialist),
        ("observer", None) => Ok(SessionAgentRole::Observer),
        _ => Err(SessionStoreError::Store(
            "stored Agent role is invalid".into(),
        )),
    }
}

const fn encode_state(state: AgentInstanceState) -> &'static str {
    match state {
        AgentInstanceState::Created => "created",
        AgentInstanceState::Ready => "ready",
        AgentInstanceState::Running => "running",
        AgentInstanceState::WaitingMessage => "waiting_message",
        AgentInstanceState::WaitingApproval => "waiting_approval",
        AgentInstanceState::Completed => "completed",
        AgentInstanceState::Failed => "failed",
        AgentInstanceState::Cancelled => "cancelled",
        AgentInstanceState::ManualReconciliation => "manual_reconciliation",
    }
}

fn decode_state(state: &str) -> Result<AgentInstanceState, SessionStoreError> {
    match state {
        "created" => Ok(AgentInstanceState::Created),
        "ready" => Ok(AgentInstanceState::Ready),
        "running" => Ok(AgentInstanceState::Running),
        "waiting_message" => Ok(AgentInstanceState::WaitingMessage),
        "waiting_approval" => Ok(AgentInstanceState::WaitingApproval),
        "completed" => Ok(AgentInstanceState::Completed),
        "failed" => Ok(AgentInstanceState::Failed),
        "cancelled" => Ok(AgentInstanceState::Cancelled),
        "manual_reconciliation" => Ok(AgentInstanceState::ManualReconciliation),
        _ => Err(SessionStoreError::Store(
            "stored Agent instance state is invalid".into(),
        )),
    }
}

fn encode_json(value: &impl serde::Serialize) -> Result<String, SessionStoreError> {
    serde_json::to_string(value).map_err(|error| SessionStoreError::Store(error.to_string()))
}

fn decode_json<T: serde::de::DeserializeOwned>(
    value: &str,
    label: &str,
) -> Result<T, SessionStoreError> {
    serde_json::from_str(value)
        .map_err(|error| SessionStoreError::Store(format!("decode {label}: {error}")))
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
#[path = "../../tests/unit/agent_instance_store.rs"]
mod tests;
