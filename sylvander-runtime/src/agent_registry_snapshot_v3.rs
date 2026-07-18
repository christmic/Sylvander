//! Pure, versioned data contract for multi-Provider Agent snapshots.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sylvander_protocol::ModelSelection;

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::agent_registry_snapshot::{
    AgentRegistrySnapshot, AgentSnapshotError, load_snapshot as load_snapshot_v2,
};

/// Qualified model policy supplied when materializing one Agent revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentSnapshotSelectionV3 {
    pub agent_id: String,
    pub agent_revision: u64,
    pub default_model: ModelSelection,
    pub allowed_models: BTreeSet<ModelSelection>,
}

impl AgentSnapshotSelectionV3 {
    pub(crate) fn validate(&self) -> Result<(), AgentSnapshotV3Error> {
        validate_agent(&self.agent_id, self.agent_revision)?;
        validate_model(&self.default_model)?;
        if self.allowed_models.is_empty() {
            return Err(AgentSnapshotV3Error::EmptyModels);
        }
        for model in &self.allowed_models {
            validate_model(model)?;
        }
        if !self.allowed_models.contains(&self.default_model) {
            return Err(AgentSnapshotV3Error::DefaultNotAllowed);
        }
        Ok(())
    }
}

/// One immutable, provider-qualified Model revision pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotModelRevisionV3 {
    pub model: ModelSelection,
    pub revision: u64,
}

/// Immutable component revisions captured for one Agent revision.
///
/// `providers` is a sorted exact Provider revision map. `models` must be
/// strictly sorted by qualified identity so serialization and digests are
/// independent of database or input ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentRegistrySnapshotV3 {
    pub agent_id: String,
    pub agent_revision: u64,
    pub default_model: ModelSelection,
    pub providers: BTreeMap<String, u64>,
    pub models: Vec<SnapshotModelRevisionV3>,
}

impl AgentRegistry {
    pub(crate) async fn stage_agent_snapshot_v3(
        &self,
        selection: AgentSnapshotSelectionV3,
    ) -> Result<AgentRegistrySnapshotV3, AgentSnapshotV3Error> {
        selection.validate()?;
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            let snapshot = stage_snapshot_v3_in_transaction(&transaction, selection)?;
            transaction.commit().map_err(storage)?;
            Ok(snapshot)
        })
        .await
    }

    pub(crate) async fn load_agent_snapshot_v3(
        &self,
        agent_id: &str,
        revision: u64,
    ) -> Result<Option<AgentRegistrySnapshotV3>, AgentSnapshotV3Error> {
        let agent_id = agent_id.to_owned();
        let revision = sql_index(revision)?;
        self.run_with(move |connection| {
            let snapshot = load_snapshot_v3(connection, &agent_id, revision)?;
            if snapshot.is_some() && has_v2_snapshot(connection, &agent_id, revision)? {
                return Err(AgentSnapshotV3Error::SchemaConflict {
                    agent_id,
                    revision: decode_index(revision)?,
                });
            }
            Ok(snapshot)
        })
        .await
    }

    pub(crate) async fn load_agent_snapshot_versioned(
        &self,
        agent_id: &str,
        revision: u64,
    ) -> Result<Option<AgentRegistrySnapshotV3>, AgentSnapshotV3Error> {
        let agent_id = agent_id.to_owned();
        let revision = sql_index(revision)?;
        self.run_with(move |connection| {
            // Validate V3 before probing V2 so damaged current state cannot be
            // hidden by a valid legacy snapshot.
            let current = load_snapshot_v3(connection, &agent_id, revision)?;
            let has_legacy = has_v2_snapshot(connection, &agent_id, revision)?;
            if let Some(current) = current {
                if has_legacy {
                    return Err(AgentSnapshotV3Error::SchemaConflict {
                        agent_id,
                        revision: decode_index(revision)?,
                    });
                }
                return Ok(Some(current));
            }
            if !has_legacy {
                return Ok(None);
            }
            load_snapshot_v2(connection, &agent_id, revision)?
                .map(lift_v2_snapshot)
                .transpose()
        })
        .await
    }
}

pub(crate) fn stage_snapshot_v3_in_transaction(
    transaction: &Transaction<'_>,
    selection: AgentSnapshotSelectionV3,
) -> Result<AgentRegistrySnapshotV3, AgentSnapshotV3Error> {
    selection.validate()?;
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
        return Err(AgentSnapshotV3Error::UnknownAgentRevision {
            agent_id: selection.agent_id,
            revision: selection.agent_revision,
        });
    }
    if has_v2_snapshot(transaction, &selection.agent_id, agent_revision)? {
        return Err(AgentSnapshotV3Error::SchemaConflict {
            agent_id: selection.agent_id,
            revision: selection.agent_revision,
        });
    }

    let mut providers = BTreeMap::new();
    let mut models = Vec::with_capacity(selection.allowed_models.len());
    for model in &selection.allowed_models {
        if !providers.contains_key(&model.provider_id) {
            let revision = active_provider_revision(transaction, &model.provider_id)?
                .ok_or_else(|| AgentSnapshotV3Error::UnknownProvider(model.provider_id.clone()))?;
            providers.insert(model.provider_id.clone(), revision);
        }
        let revision = active_model_revision(transaction, &model.provider_id, &model.model_id)?
            .ok_or_else(|| AgentSnapshotV3Error::UnknownModel {
                provider_id: model.provider_id.clone(),
                model_id: model.model_id.clone(),
            })?;
        models.push(SnapshotModelRevisionV3 {
            model: model.clone(),
            revision,
        });
    }
    let snapshot = AgentRegistrySnapshotV3::new(
        selection.agent_id,
        selection.agent_revision,
        selection.default_model,
        providers,
        models,
    )?;
    let (_, digest) = snapshot.canonical_json_and_digest()?;
    if let Some(existing) = load_snapshot_v3(transaction, &snapshot.agent_id, agent_revision)? {
        let (_, existing_digest) = existing.canonical_json_and_digest()?;
        return if digest == existing_digest {
            Ok(existing)
        } else {
            Err(AgentSnapshotV3Error::SnapshotCollision {
                agent_id: snapshot.agent_id,
                revision: snapshot.agent_revision,
            })
        };
    }

    transaction
        .execute(
            "INSERT INTO agent_registry_snapshots_v3 \
             (agent_id,agent_revision,default_provider_id,default_model_id,digest,created_at) \
             VALUES (?1,?2,?3,?4,?5,unixepoch())",
            params![
                snapshot.agent_id,
                agent_revision,
                snapshot.default_model.provider_id,
                snapshot.default_model.model_id,
                digest
            ],
        )
        .map_err(storage)?;
    for (provider_id, revision) in &snapshot.providers {
        transaction
            .execute(
                "INSERT INTO agent_registry_snapshot_providers_v3 \
                 (agent_id,agent_revision,provider_id,provider_revision) VALUES (?1,?2,?3,?4)",
                params![
                    snapshot.agent_id,
                    agent_revision,
                    provider_id,
                    sql_index(*revision)?
                ],
            )
            .map_err(storage)?;
    }
    for model in &snapshot.models {
        transaction
            .execute(
                "INSERT INTO agent_registry_snapshot_models_v3 \
                 (agent_id,agent_revision,provider_id,model_id,model_revision,is_default) \
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    snapshot.agent_id,
                    agent_revision,
                    model.model.provider_id,
                    model.model.model_id,
                    sql_index(model.revision)?,
                    model.model == snapshot.default_model
                ],
            )
            .map_err(storage)?;
    }
    Ok(snapshot)
}

impl AgentRegistrySnapshotV3 {
    pub(crate) fn new(
        agent_id: String,
        agent_revision: u64,
        default_model: ModelSelection,
        providers: BTreeMap<String, u64>,
        mut models: Vec<SnapshotModelRevisionV3>,
    ) -> Result<Self, AgentSnapshotV3Error> {
        models.sort_by(|left, right| left.model.cmp(&right.model));
        let snapshot = Self {
            agent_id,
            agent_revision,
            default_model,
            providers,
            models,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub(crate) fn validate(&self) -> Result<(), AgentSnapshotV3Error> {
        validate_agent(&self.agent_id, self.agent_revision)?;
        validate_model(&self.default_model)?;
        if self.providers.is_empty() {
            return Err(AgentSnapshotV3Error::EmptyProviders);
        }
        for (provider_id, revision) in &self.providers {
            if provider_id.trim().is_empty() || *revision == 0 {
                return Err(AgentSnapshotV3Error::InvalidProvider);
            }
        }
        if self.models.is_empty() {
            return Err(AgentSnapshotV3Error::EmptyModels);
        }
        for model in &self.models {
            validate_model(&model.model)?;
            if model.revision == 0 {
                return Err(AgentSnapshotV3Error::InvalidModelRevision);
            }
            if !self.providers.contains_key(&model.model.provider_id) {
                return Err(AgentSnapshotV3Error::MissingProviderPin(
                    model.model.provider_id.clone(),
                ));
            }
        }
        let model_providers = self
            .models
            .iter()
            .map(|model| model.model.provider_id.as_str())
            .collect::<BTreeSet<_>>();
        if model_providers
            != self
                .providers
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        {
            return Err(AgentSnapshotV3Error::ProviderSetMismatch);
        }
        for pair in self.models.windows(2) {
            match pair[0].model.cmp(&pair[1].model) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => {
                    return Err(AgentSnapshotV3Error::DuplicateModel {
                        provider_id: pair[0].model.provider_id.clone(),
                        model_id: pair[0].model.model_id.clone(),
                    });
                }
                std::cmp::Ordering::Greater => {
                    return Err(AgentSnapshotV3Error::ModelsNotSorted);
                }
            }
        }
        if !self
            .models
            .iter()
            .any(|model| model.model == self.default_model)
        {
            return Err(AgentSnapshotV3Error::DefaultNotAllowed);
        }
        Ok(())
    }

    /// Serialize the validated, deterministically ordered contract and hash it.
    pub(crate) fn canonical_json_and_digest(
        &self,
    ) -> Result<(String, String), AgentSnapshotV3Error> {
        self.validate()?;
        let json = serde_json::to_string(self)
            .map_err(|error| AgentSnapshotV3Error::Serialization(error.to_string()))?;
        let mut hasher = Sha256::new();
        hasher.update(b"sylvander.agent-registry-snapshot/v3\0");
        hasher.update(json.as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        Ok((json, digest))
    }
}

fn load_snapshot_v3(
    connection: &Connection,
    agent_id: &str,
    revision: i64,
) -> Result<Option<AgentRegistrySnapshotV3>, AgentSnapshotV3Error> {
    let header = connection
        .query_row(
            "SELECT default_provider_id,default_model_id,digest \
             FROM agent_registry_snapshots_v3 WHERE agent_id=?1 AND agent_revision=?2",
            params![agent_id, revision],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    let Some((default_provider_id, default_model_id, stored_digest)) = header else {
        return Ok(None);
    };
    let mut provider_statement = connection
        .prepare(
            "SELECT provider_id,provider_revision \
             FROM agent_registry_snapshot_providers_v3 \
             WHERE agent_id=?1 AND agent_revision=?2 ORDER BY provider_id",
        )
        .map_err(storage)?;
    let providers = provider_statement
        .query_map(params![agent_id, revision], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(storage)?
        .map(|row| {
            let (provider_id, revision) = row.map_err(storage)?;
            Ok((provider_id, decode_index(revision)?))
        })
        .collect::<Result<BTreeMap<_, _>, AgentSnapshotV3Error>>()?;

    let mut model_statement = connection
        .prepare(
            "SELECT provider_id,model_id,model_revision,is_default \
             FROM agent_registry_snapshot_models_v3 \
             WHERE agent_id=?1 AND agent_revision=?2 ORDER BY provider_id,model_id",
        )
        .map_err(storage)?;
    let mut default_rows = 0;
    let models = model_statement
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
            let model = ModelSelection {
                provider_id,
                model_id,
            };
            if is_default {
                default_rows += 1;
                if model.provider_id != default_provider_id || model.model_id != default_model_id {
                    return Err(integrity("V3 snapshot default Model mismatch"));
                }
            }
            Ok(SnapshotModelRevisionV3 {
                model,
                revision: decode_index(revision)?,
            })
        })
        .collect::<Result<Vec<_>, AgentSnapshotV3Error>>()?;
    if default_rows != 1 {
        return Err(integrity(
            "V3 snapshot must contain exactly one default Model row",
        ));
    }
    let snapshot = AgentRegistrySnapshotV3::new(
        agent_id.to_owned(),
        decode_index(revision)?,
        ModelSelection {
            provider_id: default_provider_id,
            model_id: default_model_id,
        },
        providers,
        models,
    )
    .map_err(|_| integrity("V3 snapshot contract is invalid"))?;
    let (_, actual_digest) = snapshot.canonical_json_and_digest()?;
    if actual_digest != stored_digest {
        return Err(integrity("V3 snapshot digest mismatch"));
    }
    Ok(Some(snapshot))
}

fn lift_v2_snapshot(
    legacy: AgentRegistrySnapshot,
) -> Result<AgentRegistrySnapshotV3, AgentSnapshotV3Error> {
    let default_model = legacy
        .models
        .iter()
        .find(|model| model.is_default)
        .map(|model| ModelSelection {
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
        })
        .ok_or_else(|| integrity("V2 snapshot has no default Model"))?;
    AgentRegistrySnapshotV3::new(
        legacy.agent_id,
        legacy.agent_revision,
        default_model,
        BTreeMap::from([(legacy.provider_id, legacy.provider_revision)]),
        legacy
            .models
            .into_iter()
            .map(|model| SnapshotModelRevisionV3 {
                model: ModelSelection {
                    provider_id: model.provider_id,
                    model_id: model.model_id,
                },
                revision: model.revision,
            })
            .collect(),
    )
    .map_err(|_| integrity("V2 snapshot cannot be represented as V3"))
}

fn active_provider_revision(
    connection: &Connection,
    provider_id: &str,
) -> Result<Option<u64>, AgentSnapshotV3Error> {
    connection
        .query_row(
            "SELECT h.active_revision FROM provider_registry_heads h \
             JOIN provider_definitions d ON d.provider_id=h.provider_id \
              AND d.revision=h.active_revision WHERE h.provider_id=?1",
            [provider_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(storage)?
        .map(decode_index)
        .transpose()
}

fn has_v2_snapshot(
    connection: &Connection,
    agent_id: &str,
    revision: i64,
) -> Result<bool, AgentSnapshotV3Error> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_registry_snapshots \
             WHERE agent_id=?1 AND agent_revision=?2)",
            params![agent_id, revision],
            |row| row.get(0),
        )
        .map_err(storage)
}

fn active_model_revision(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<Option<u64>, AgentSnapshotV3Error> {
    connection
        .query_row(
            "SELECT h.active_revision FROM model_registry_heads h \
             JOIN model_definitions d ON d.provider_id=h.provider_id \
              AND d.model_id=h.model_id AND d.revision=h.active_revision \
             WHERE h.provider_id=?1 AND h.model_id=?2",
            params![provider_id, model_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(storage)?
        .map(decode_index)
        .transpose()
}

fn sql_index(value: u64) -> Result<i64, AgentSnapshotV3Error> {
    i64::try_from(value).map_err(|_| AgentSnapshotV3Error::InvalidRevision)
}

fn decode_index(value: i64) -> Result<u64, AgentSnapshotV3Error> {
    u64::try_from(value).map_err(|_| integrity("V3 snapshot revision is negative"))
}

fn storage(error: rusqlite::Error) -> AgentSnapshotV3Error {
    AgentRegistryError::sqlite(error).into()
}

fn integrity(message: &str) -> AgentSnapshotV3Error {
    AgentRegistryError::Integrity(message.into()).into()
}

fn validate_agent(agent_id: &str, revision: u64) -> Result<(), AgentSnapshotV3Error> {
    if agent_id.trim().is_empty() || revision == 0 {
        Err(AgentSnapshotV3Error::InvalidAgent)
    } else {
        Ok(())
    }
}

fn validate_model(model: &ModelSelection) -> Result<(), AgentSnapshotV3Error> {
    if model.provider_id.trim().is_empty() || model.model_id.trim().is_empty() {
        Err(AgentSnapshotV3Error::InvalidModel)
    } else {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AgentSnapshotV3Error {
    #[error("invalid Agent snapshot identity or revision")]
    InvalidAgent,
    #[error("Agent snapshot must pin at least one Provider")]
    EmptyProviders,
    #[error("invalid Provider identity or revision")]
    InvalidProvider,
    #[error("Agent snapshot must allow at least one Model")]
    EmptyModels,
    #[error("invalid qualified Model identity")]
    InvalidModel,
    #[error("invalid Model revision")]
    InvalidModelRevision,
    #[error("Model Provider `{0}` has no exact revision pin")]
    MissingProviderPin(String),
    #[error("Provider pins do not exactly match the qualified Model catalog")]
    ProviderSetMismatch,
    #[error("duplicate qualified Model `{provider_id}/{model_id}`")]
    DuplicateModel {
        provider_id: String,
        model_id: String,
    },
    #[error("qualified Models are not canonically sorted")]
    ModelsNotSorted,
    #[error("default Model is not in the allowed qualified catalog")]
    DefaultNotAllowed,
    #[error("Agent definition and snapshot selection do not match")]
    DefinitionSelectionMismatch,
    #[error("failed to serialize Agent snapshot: {0}")]
    Serialization(String),
    #[error("invalid snapshot revision")]
    InvalidRevision,
    #[error("unknown Agent revision `{agent_id}`@{revision}")]
    UnknownAgentRevision { agent_id: String, revision: u64 },
    #[error("unknown Provider `{0}`")]
    UnknownProvider(String),
    #[error("unknown Model `{provider_id}/{model_id}`")]
    UnknownModel {
        provider_id: String,
        model_id: String,
    },
    #[error("Agent snapshot `{agent_id}`@{revision} already uses the V2 schema")]
    SchemaConflict { agent_id: String, revision: u64 },
    #[error("Agent snapshot `{agent_id}`@{revision} has different V3 content")]
    SnapshotCollision { agent_id: String, revision: u64 },
    #[error(transparent)]
    Registry(#[from] AgentRegistryError),
    #[error(transparent)]
    Legacy(#[from] AgentSnapshotError),
}

#[cfg(test)]
#[path = "../tests/unit/agent_registry_snapshot_v3.rs"]
mod tests;
