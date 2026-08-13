//! SQLite persistence for governed multi-Agent coordination facts.

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use sylvander_api::{AgentInstanceId, SessionId};

use crate::coordination::topology::{AgentRelation, AgentRelationKind, SessionTopology};
use crate::session::membership::SessionMembership;
use crate::storage::session::{SessionStoreError, SqliteSessionStore};

#[async_trait]
pub trait CoordinationStore: Send + Sync {
    async fn save_topology(
        &self,
        topology: &SessionTopology,
        membership: &SessionMembership,
        expected_revision: Option<u64>,
    ) -> Result<(), SessionStoreError>;

    async fn topology(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionTopology>, SessionStoreError>;
}

#[async_trait]
impl CoordinationStore for SqliteSessionStore {
    async fn save_topology(
        &self,
        topology: &SessionTopology,
        membership: &SessionMembership,
        expected_revision: Option<u64>,
    ) -> Result<(), SessionStoreError> {
        topology
            .validate(membership)
            .map_err(|error| SessionStoreError::Invalid(error.to_string()))?;
        let topology = topology.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let membership_revision = transaction
                .query_row(
                    "SELECT membership_revision FROM session_governance WHERE session_id=?1",
                    [&topology.session_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            let Some(membership_revision) = membership_revision else {
                return Err(SessionStoreError::Invalid(
                    "topology requires durable Session membership".into(),
                ));
            };
            let actual_membership_revision =
                checked_u64(membership_revision, "membership revision")?;
            if actual_membership_revision != topology.membership_revision {
                return Err(SessionStoreError::MembershipConflict {
                    expected: Some(topology.membership_revision),
                    actual: Some(actual_membership_revision),
                });
            }
            let actual_revision = transaction
                .query_row(
                    "SELECT topology_revision FROM session_topology WHERE session_id=?1",
                    [&topology.session_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .map(|value| checked_u64(value, "topology revision"))
                .transpose()?;
            if actual_revision != expected_revision {
                return Err(SessionStoreError::TopologyConflict {
                    expected: expected_revision,
                    actual: actual_revision,
                });
            }
            let next_revision = match expected_revision {
                None => 0,
                Some(revision) => revision.checked_add(1).ok_or_else(|| {
                    SessionStoreError::Invalid("topology revision overflow".into())
                })?,
            };
            if topology.topology_revision != next_revision {
                return Err(SessionStoreError::Invalid(
                    "next topology revision is not sequential".into(),
                ));
            }

            transaction.execute(
                "DELETE FROM session_topology WHERE session_id=?1",
                [&topology.session_id.0],
            )?;
            transaction.execute(
                "INSERT INTO session_topology \
                 (session_id,membership_revision,topology_revision,updated_at) \
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
                    "INSERT INTO agent_relations \
                     (session_id,relation_ordinal,source_instance_id,target_instance_id,relation_kind,created_at) \
                     VALUES (?1,?2,?3,?4,?5,?6)",
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

    async fn topology(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionTopology>, SessionStoreError> {
        let session_id = session_id.clone();
        self.run(move |connection| {
            let header = connection
                .query_row(
                    "SELECT membership_revision,topology_revision,updated_at \
                     FROM session_topology WHERE session_id=?1",
                    [&session_id.0],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((membership_revision, topology_revision, updated_at)) = header else {
                return Ok(None);
            };
            let mut statement = connection.prepare(
                "SELECT source_instance_id,target_instance_id,relation_kind,created_at \
                 FROM agent_relations WHERE session_id=?1 ORDER BY relation_ordinal",
            )?;
            let rows = statement.query_map([&session_id.0], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            let relations = rows
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|(source, target, kind, created_at)| {
                    Ok(AgentRelation {
                        source: AgentInstanceId::new(source),
                        target: AgentInstanceId::new(target),
                        kind: decode_relation_kind(&kind)?,
                        created_at,
                    })
                })
                .collect::<Result<Vec<_>, SessionStoreError>>()?;
            Ok(Some(SessionTopology {
                session_id,
                membership_revision: checked_u64(membership_revision, "membership revision")?,
                topology_revision: checked_u64(topology_revision, "topology revision")?,
                relations,
                updated_at,
            }))
        })
        .await
    }
}

const fn encode_relation_kind(kind: AgentRelationKind) -> &'static str {
    match kind {
        AgentRelationKind::ParentOf => "parent_of",
        AgentRelationKind::Peer => "peer",
        AgentRelationKind::Reviews => "reviews",
    }
}

fn decode_relation_kind(kind: &str) -> Result<AgentRelationKind, SessionStoreError> {
    match kind {
        "parent_of" => Ok(AgentRelationKind::ParentOf),
        "peer" => Ok(AgentRelationKind::Peer),
        "reviews" => Ok(AgentRelationKind::Reviews),
        _ => Err(SessionStoreError::Store(
            "stored Agent relation kind is invalid".into(),
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
