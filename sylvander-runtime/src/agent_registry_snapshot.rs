use std::collections::{BTreeSet, HashSet};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::agent_registry::{AgentRegistry, AgentRegistryError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSnapshotSelection {
    pub agent_id: String,
    pub agent_revision: u64,
    pub provider_id: String,
    pub allowed_model_ids: BTreeSet<String>,
    pub default_model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AgentRegistrySnapshot {
    pub agent_id: String,
    pub agent_revision: u64,
    pub provider_id: String,
    pub provider_revision: u64,
    pub models: Vec<SnapshotModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SnapshotModel {
    pub provider_id: String,
    pub model_id: String,
    pub revision: u64,
    pub is_default: bool,
}

impl AgentRegistrySnapshot {
    pub(crate) fn validate(&self) -> Result<(), AgentSnapshotError> {
        validate_snapshot(self).map_err(AgentSnapshotError::Integrity)
    }
}

impl AgentRegistry {
    pub(crate) async fn stage_agent_snapshot(
        &self,
        selection: AgentSnapshotSelection,
    ) -> Result<AgentRegistrySnapshot, AgentSnapshotError> {
        validate_selection(&selection)?;
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            let agent_revision = sql_index(selection.agent_revision)?;
            let agent_exists = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM agent_definitions \
                     WHERE agent_id=?1 AND revision=?2)",
                    params![selection.agent_id, agent_revision],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(storage)?;
            if !agent_exists {
                return Err(AgentSnapshotError::UnknownAgentRevision {
                    agent_id: selection.agent_id,
                    revision: selection.agent_revision,
                });
            }
            let has_v3 = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM agent_registry_snapshots_v3 \
                     WHERE agent_id=?1 AND agent_revision=?2)",
                    params![selection.agent_id, agent_revision],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(storage)?;
            if has_v3 {
                return Err(AgentSnapshotError::SnapshotSchemaConflict {
                    agent_id: selection.agent_id,
                    revision: selection.agent_revision,
                });
            }
            let provider_revision = active_head(
                &transaction,
                "provider_registry_heads",
                &selection.provider_id,
                None,
            )?
            .ok_or_else(|| AgentSnapshotError::UnknownProvider(selection.provider_id.clone()))?;
            let mut models = Vec::with_capacity(selection.allowed_model_ids.len());
            for model_id in &selection.allowed_model_ids {
                let revision = active_head(
                    &transaction,
                    "model_registry_heads",
                    &selection.provider_id,
                    Some(model_id),
                )?
                .ok_or_else(|| AgentSnapshotError::UnknownModel {
                    provider_id: selection.provider_id.clone(),
                    model_id: model_id.clone(),
                })?;
                models.push(SnapshotModel {
                    provider_id: selection.provider_id.clone(),
                    model_id: model_id.clone(),
                    revision,
                    is_default: model_id == &selection.default_model_id,
                });
            }
            let snapshot = AgentRegistrySnapshot {
                agent_id: selection.agent_id,
                agent_revision: selection.agent_revision,
                provider_id: selection.provider_id,
                provider_revision,
                models,
            };
            snapshot.validate()?;
            let digest = snapshot_digest(&snapshot)?;
            if let Some(existing) = load_snapshot(&transaction, &snapshot.agent_id, agent_revision)?
            {
                return if snapshot_digest(&existing)? == digest {
                    transaction.commit().map_err(storage)?;
                    Ok(existing)
                } else {
                    Err(AgentSnapshotError::SnapshotCollision {
                        agent_id: snapshot.agent_id,
                        revision: snapshot.agent_revision,
                    })
                };
            }
            transaction
                .execute(
                    "INSERT INTO agent_registry_snapshots \
                     (agent_id,agent_revision,provider_id,provider_revision,digest,created_at) \
                     VALUES (?1,?2,?3,?4,?5,unixepoch())",
                    params![
                        snapshot.agent_id,
                        agent_revision,
                        snapshot.provider_id,
                        sql_index(snapshot.provider_revision)?,
                        digest
                    ],
                )
                .map_err(storage)?;
            for model in &snapshot.models {
                transaction
                    .execute(
                        "INSERT INTO agent_registry_snapshot_models \
                         (agent_id,agent_revision,provider_id,model_id,model_revision,is_default) \
                         VALUES (?1,?2,?3,?4,?5,?6)",
                        params![
                            snapshot.agent_id,
                            agent_revision,
                            model.provider_id,
                            model.model_id,
                            sql_index(model.revision)?,
                            model.is_default
                        ],
                    )
                    .map_err(storage)?;
            }
            transaction.commit().map_err(storage)?;
            Ok(snapshot)
        })
        .await
    }

    pub(crate) async fn load_agent_snapshot(
        &self,
        agent_id: &str,
        revision: u64,
    ) -> Result<Option<AgentRegistrySnapshot>, AgentSnapshotError> {
        let agent_id = agent_id.to_owned();
        let revision = sql_index(revision)?;
        self.run_with(move |connection| load_snapshot(connection, &agent_id, revision))
            .await
    }
}

pub(crate) fn load_snapshot(
    connection: &Connection,
    agent_id: &str,
    revision: i64,
) -> Result<Option<AgentRegistrySnapshot>, AgentSnapshotError> {
    let header = connection
        .query_row(
            "SELECT provider_id,provider_revision,digest FROM agent_registry_snapshots \
             WHERE agent_id=?1 AND agent_revision=?2",
            params![agent_id, revision],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    let Some((provider_id, provider_revision, expected_digest)) = header else {
        return Ok(None);
    };
    let mut statement = connection
        .prepare(
            "SELECT provider_id,model_id,model_revision,is_default \
             FROM agent_registry_snapshot_models WHERE agent_id=?1 AND agent_revision=?2 \
             ORDER BY provider_id,model_id",
        )
        .map_err(storage)?;
    let models = statement
        .query_map(params![agent_id, revision], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })
        .map_err(storage)?
        .map(|row| {
            let (provider_id, model_id, revision, is_default) = row.map_err(storage)?;
            Ok(SnapshotModel {
                provider_id,
                model_id,
                revision: decode_index(revision)?,
                is_default,
            })
        })
        .collect::<Result<Vec<_>, AgentSnapshotError>>()?;
    let snapshot = AgentRegistrySnapshot {
        agent_id: agent_id.to_owned(),
        agent_revision: decode_index(revision)?,
        provider_id,
        provider_revision: decode_index(provider_revision)?,
        models,
    };
    snapshot.validate()?;
    if snapshot_digest(&snapshot)? != expected_digest {
        return Err(AgentSnapshotError::Integrity(
            "Agent registry snapshot digest mismatch".into(),
        ));
    }
    Ok(Some(snapshot))
}

fn validate_selection(selection: &AgentSnapshotSelection) -> Result<(), AgentSnapshotError> {
    if selection.agent_revision == 0
        || selection.agent_id.trim().is_empty()
        || selection.provider_id.trim().is_empty()
        || selection.default_model_id.trim().is_empty()
    {
        return Err(AgentSnapshotError::InvalidIdentity);
    }
    if selection.allowed_model_ids.is_empty() {
        return Err(AgentSnapshotError::EmptyModels);
    }
    if !selection
        .allowed_model_ids
        .contains(&selection.default_model_id)
    {
        return Err(AgentSnapshotError::DefaultNotAllowed(
            selection.default_model_id.clone(),
        ));
    }
    if selection
        .allowed_model_ids
        .iter()
        .any(|id| id.trim().is_empty())
    {
        return Err(AgentSnapshotError::InvalidIdentity);
    }
    Ok(())
}

fn validate_snapshot(snapshot: &AgentRegistrySnapshot) -> Result<(), String> {
    if snapshot.agent_revision == 0
        || snapshot.provider_revision == 0
        || snapshot.agent_id.trim().is_empty()
        || snapshot.provider_id.trim().is_empty()
        || snapshot.models.is_empty()
    {
        return Err("invalid Agent registry snapshot identity".into());
    }
    let mut identities = HashSet::new();
    let mut defaults = 0;
    for model in &snapshot.models {
        if model.provider_id != snapshot.provider_id
            || model.model_id.trim().is_empty()
            || model.revision == 0
            || !identities.insert((&model.provider_id, &model.model_id))
        {
            return Err("invalid or duplicate qualified Model binding".into());
        }
        defaults += usize::from(model.is_default);
    }
    if defaults != 1 {
        return Err("Agent registry snapshot must have exactly one default Model".into());
    }
    Ok(())
}

fn active_head(
    connection: &Connection,
    table: &str,
    provider_id: &str,
    model_id: Option<&str>,
) -> Result<Option<u64>, AgentSnapshotError> {
    let sql = if model_id.is_some() {
        format!("SELECT active_revision FROM {table} WHERE provider_id=?1 AND model_id=?2")
    } else {
        format!("SELECT active_revision FROM {table} WHERE provider_id=?1")
    };
    let stored = match model_id {
        Some(model_id) => connection.query_row(&sql, params![provider_id, model_id], |row| {
            row.get::<_, i64>(0)
        }),
        None => connection.query_row(&sql, [provider_id], |row| row.get::<_, i64>(0)),
    }
    .optional()
    .map_err(storage)?;
    stored.map(decode_index).transpose()
}

fn snapshot_digest(snapshot: &AgentRegistrySnapshot) -> Result<String, AgentSnapshotError> {
    let json = serde_json::to_vec(snapshot).map_err(AgentRegistryError::serde)?;
    Ok(format!("{:x}", Sha256::digest(json)))
}

fn sql_index(value: u64) -> Result<i64, AgentSnapshotError> {
    i64::try_from(value).map_err(|_| AgentSnapshotError::InvalidIdentity)
}

fn decode_index(value: i64) -> Result<u64, AgentSnapshotError> {
    u64::try_from(value)
        .map_err(|_| AgentSnapshotError::Integrity("stored revision is negative".into()))
}

fn storage(error: rusqlite::Error) -> AgentSnapshotError {
    AgentRegistryError::sqlite(error).into()
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AgentSnapshotError {
    #[error("invalid Agent registry snapshot identity")]
    InvalidIdentity,
    #[error("Agent registry snapshot must allow at least one Model")]
    EmptyModels,
    #[error("default Model `{0}` is not in the allowed catalog")]
    DefaultNotAllowed(String),
    #[error("unknown Agent revision `{agent_id}`@{revision}")]
    UnknownAgentRevision { agent_id: String, revision: u64 },
    #[error("unknown Provider `{0}`")]
    UnknownProvider(String),
    #[error("unknown Model `{provider_id}/{model_id}`")]
    UnknownModel {
        provider_id: String,
        model_id: String,
    },
    #[error("Agent registry snapshot `{agent_id}`@{revision} already has different bindings")]
    SnapshotCollision { agent_id: String, revision: u64 },
    #[error("Agent registry snapshot `{agent_id}`@{revision} already uses the V3 schema")]
    SnapshotSchemaConflict { agent_id: String, revision: u64 },
    #[error("Agent registry snapshot integrity error: {0}")]
    Integrity(String),
    #[error(transparent)]
    Registry(#[from] AgentRegistryError),
}
