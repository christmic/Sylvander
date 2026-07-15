use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::registry_domain::{ModelDefinition, StoredRevision, canonical_definition};

const MAX_MODEL_PAGE_SIZE: u16 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelRevisionPage {
    pub active_revision: u64,
    pub revisions: Vec<StoredRevision<ModelDefinition>>,
    pub next_before_revision: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ModelRegistryError {
    #[error("invalid Model definition")]
    InvalidDefinition,
    #[error("unknown Provider `{0}`")]
    UnknownProvider(String),
    #[error("unknown Model `{0}`")]
    UnknownModel(String),
    #[error("unknown Model revision `{identity}`@{revision}")]
    UnknownRevision { identity: String, revision: u64 },
    #[error("Model `{identity}` active revision conflict: expected {expected}, found {actual}")]
    Conflict {
        identity: String,
        expected: u64,
        actual: u64,
    },
    #[error("Model `{identity}` next revision must be {expected}, found {actual}")]
    NonSequential {
        identity: String,
        expected: u64,
        actual: u64,
    },
    #[error("Model `{identity}` revision {revision} has different content")]
    RevisionCollision { identity: String, revision: u64 },
    #[error("Model rollback target {target} is not older than active revision {actual}")]
    InvalidRollback { target: u64, actual: u64 },
    #[error(transparent)]
    Registry(#[from] AgentRegistryError),
}

impl AgentRegistry {
    pub(crate) async fn seed_model(
        &self,
        definition: ModelDefinition,
    ) -> Result<StoredRevision<ModelDefinition>, ModelRegistryError> {
        validate(&definition)?;
        if definition.revision != 1 {
            return Err(ModelRegistryError::NonSequential {
                identity: identity(&definition.provider_id, &definition.model_id),
                expected: 1,
                actual: definition.revision,
            });
        }
        let provider_id = definition.provider_id.clone();
        let model_id = definition.model_id.clone();
        let result_identity = (provider_id.clone(), model_id.clone());
        let (json, digest) = canonical_definition(&definition)?;
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            require_provider(&transaction, &provider_id)?;
            if active_revision(&transaction, &provider_id, &model_id)?.is_some() {
                transaction.commit().map_err(storage)?;
                return Ok(());
            }
            insert_definition(&transaction, &definition, &json, &digest)?;
            transaction
                .execute(
                    "INSERT INTO model_registry_heads \
                     (provider_id,model_id,active_revision,updated_at) \
                     VALUES (?1,?2,1,unixepoch())",
                    params![provider_id, model_id],
                )
                .map_err(storage)?;
            transaction.commit().map_err(storage)
        })
        .await?;
        self.load_active_model((&result_identity.0, &result_identity.1))
            .await?
            .ok_or_else(|| {
                ModelRegistryError::UnknownModel(identity(&result_identity.0, &result_identity.1))
            })
    }

    pub(crate) async fn stage_model(
        &self,
        expected_active: u64,
        definition: ModelDefinition,
    ) -> Result<StoredRevision<ModelDefinition>, ModelRegistryError> {
        validate(&definition)?;
        let provider_id = definition.provider_id.clone();
        let model_id = definition.model_id.clone();
        let result_identity = (provider_id.clone(), model_id.clone());
        let revision = definition.revision;
        let (json, digest) = canonical_definition(&definition)?;
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            let actual =
                active_revision(&transaction, &provider_id, &model_id)?.ok_or_else(|| {
                    ModelRegistryError::UnknownModel(identity(&provider_id, &model_id))
                })?;
            if actual != expected_active {
                return Err(ModelRegistryError::Conflict {
                    identity: identity(&provider_id, &model_id),
                    expected: expected_active,
                    actual,
                });
            }
            if let Some(existing) =
                definition_digest(&transaction, &provider_id, &model_id, revision)?
            {
                return if existing == digest {
                    transaction.commit().map_err(storage)
                } else {
                    Err(ModelRegistryError::RevisionCollision {
                        identity: identity(&provider_id, &model_id),
                        revision,
                    })
                };
            }
            let latest = latest_revision(&transaction, &provider_id, &model_id)?;
            if revision != latest + 1 {
                return Err(ModelRegistryError::NonSequential {
                    identity: identity(&provider_id, &model_id),
                    expected: latest + 1,
                    actual: revision,
                });
            }
            insert_definition(&transaction, &definition, &json, &digest)?;
            transaction.commit().map_err(storage)
        })
        .await?;
        self.load_model_revision(&result_identity.0, &result_identity.1, revision)
            .await?
            .ok_or(ModelRegistryError::UnknownRevision {
                identity: identity(&result_identity.0, &result_identity.1),
                revision,
            })
    }

    pub(crate) async fn activate_model(
        &self,
        key: (&str, &str),
        revision: u64,
        expected_active: u64,
    ) -> Result<(), ModelRegistryError> {
        set_head(self, key, revision, expected_active, false).await
    }

    pub(crate) async fn rollback_model(
        &self,
        key: (&str, &str),
        target_revision: u64,
        expected_active: u64,
    ) -> Result<(), ModelRegistryError> {
        set_head(self, key, target_revision, expected_active, true).await
    }

    pub(crate) async fn load_active_model(
        &self,
        key: (&str, &str),
    ) -> Result<Option<StoredRevision<ModelDefinition>>, ModelRegistryError> {
        let provider = key.0.to_owned();
        let model = key.1.to_owned();
        let revision = self
            .run_with(move |connection| active_revision(connection, &provider, &model))
            .await?;
        match revision {
            Some(revision) => self
                .load_model_revision(key.0, key.1, revision)
                .await
                .map_err(Into::into),
            None => Ok(None),
        }
    }

    pub(crate) async fn inspect_model(
        &self,
        key: (&str, &str),
    ) -> Result<Vec<StoredRevision<ModelDefinition>>, ModelRegistryError> {
        let provider = key.0.to_owned();
        let model = key.1.to_owned();
        let revisions: Vec<u64> = self
            .run_with(move |connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT revision FROM model_definitions WHERE provider_id=?1 \
                         AND model_id=?2 ORDER BY revision DESC",
                    )
                    .map_err(storage)?;
                statement
                    .query_map(params![provider, model], |row| row.get::<_, i64>(0))
                    .map_err(storage)?
                    .map(|row| row.map_err(storage).and_then(sql_revision))
                    .collect()
            })
            .await?;
        let mut stored = Vec::with_capacity(revisions.len());
        for revision in revisions {
            if let Some(item) = self.load_model_revision(key.0, key.1, revision).await? {
                stored.push(item);
            }
        }
        Ok(stored)
    }

    /// Inspect one bounded page for an exact qualified Model identity.
    pub(crate) async fn inspect_model_page(
        &self,
        key: (&str, &str),
        before_revision: Option<u64>,
        limit: u16,
    ) -> Result<ModelRevisionPage, ModelRegistryError> {
        if !(1..=MAX_MODEL_PAGE_SIZE).contains(&limit) || before_revision == Some(0) {
            return Err(ModelRegistryError::InvalidDefinition);
        }
        let provider_id = key.0.to_owned();
        let model_id = key.1.to_owned();
        let before = before_revision.map(sql_index).transpose()?;
        let sql_limit = i64::from(limit) + 1;
        self.run_with(move |connection| {
            let mut statement = connection
                .prepare(
                    "WITH state AS (
                       SELECT
                         (SELECT active_revision FROM model_registry_heads
                          WHERE provider_id=?1 AND model_id=?2) AS active_revision,
                         EXISTS(SELECT 1 FROM model_definitions
                                WHERE provider_id=?1 AND model_id=?2) AS has_revisions,
                         EXISTS(
                           SELECT 1 FROM model_registry_heads h
                           JOIN model_definitions d
                             ON d.provider_id=h.provider_id AND d.model_id=h.model_id
                            AND d.revision=h.active_revision
                           WHERE h.provider_id=?1 AND h.model_id=?2
                         ) AS active_exists
                     )
                     SELECT state.active_revision, state.has_revisions, state.active_exists,
                            d.revision, d.definition_json, d.digest, d.created_at
                     FROM state
                     LEFT JOIN model_definitions d
                       ON d.provider_id=?1 AND d.model_id=?2
                      AND (?3 IS NULL OR d.revision < ?3)
                     ORDER BY d.revision DESC
                     LIMIT ?4",
                )
                .map_err(storage)?;
            let mut rows = statement
                .query(params![provider_id, model_id, before, sql_limit])
                .map_err(storage)?;
            let mut active_revision = None;
            let mut revisions = Vec::with_capacity(usize::from(limit) + 1);
            while let Some(row) = rows.next().map_err(storage)? {
                let active = row.get::<_, Option<i64>>(0).map_err(storage)?;
                let has_revisions = row.get::<_, bool>(1).map_err(storage)?;
                let active_exists = row.get::<_, bool>(2).map_err(storage)?;
                let Some(active) = active else {
                    return if has_revisions {
                        Err(
                            AgentRegistryError::Integrity("Model registry head is missing".into())
                                .into(),
                        )
                    } else {
                        Err(ModelRegistryError::UnknownModel(identity(
                            &provider_id,
                            &model_id,
                        )))
                    };
                };
                if !active_exists {
                    return Err(AgentRegistryError::Integrity(
                        "Model active revision is missing".into(),
                    )
                    .into());
                }
                let active = sql_revision(active)?;
                active_revision = Some(active);
                let Some(revision) = row.get::<_, Option<i64>>(3).map_err(storage)? else {
                    continue;
                };
                revisions.push(decode_page_revision(
                    &provider_id,
                    &model_id,
                    revision,
                    required_column(row.get(4).map_err(storage)?)?,
                    required_column(row.get(5).map_err(storage)?)?,
                    required_column(row.get(6).map_err(storage)?)?,
                    active,
                )?);
            }
            let active_revision = active_revision.ok_or_else(|| {
                ModelRegistryError::UnknownModel(identity(&provider_id, &model_id))
            })?;
            let next_before_revision = (revisions.len() > usize::from(limit))
                .then(|| revisions[usize::from(limit) - 1].definition.revision);
            revisions.truncate(usize::from(limit));
            Ok(ModelRevisionPage {
                active_revision,
                revisions,
                next_before_revision,
            })
        })
        .await
    }
}

fn required_column<T>(value: Option<T>) -> Result<T, ModelRegistryError> {
    value.ok_or_else(|| {
        AgentRegistryError::Integrity("Model revision row is incomplete".into()).into()
    })
}

fn decode_page_revision(
    provider_id: &str,
    model_id: &str,
    revision: i64,
    definition_json: String,
    stored_digest: String,
    created_at: i64,
    active_revision: u64,
) -> Result<StoredRevision<ModelDefinition>, ModelRegistryError> {
    let revision = sql_revision(revision)?;
    let definition: ModelDefinition =
        serde_json::from_str(&definition_json).map_err(AgentRegistryError::serde)?;
    if definition.provider_id != provider_id
        || definition.model_id != model_id
        || definition.revision != revision
        || definition.validate().is_err()
    {
        return Err(
            AgentRegistryError::Integrity("stored Model identity is invalid".into()).into(),
        );
    }
    let (_, expected_digest) = canonical_definition(&definition)?;
    if stored_digest != expected_digest {
        return Err(
            AgentRegistryError::Integrity("Model definition digest mismatch".into()).into(),
        );
    }
    Ok(StoredRevision {
        definition,
        digest: stored_digest,
        created_at,
        active: revision == active_revision,
    })
}

async fn set_head(
    registry: &AgentRegistry,
    key: (&str, &str),
    target: u64,
    expected: u64,
    rollback: bool,
) -> Result<(), ModelRegistryError> {
    let provider_id = key.0.to_owned();
    let model_id = key.1.to_owned();
    registry
        .run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            let actual =
                active_revision(&transaction, &provider_id, &model_id)?.ok_or_else(|| {
                    ModelRegistryError::UnknownModel(identity(&provider_id, &model_id))
                })?;
            if actual != expected {
                return Err(ModelRegistryError::Conflict {
                    identity: identity(&provider_id, &model_id),
                    expected,
                    actual,
                });
            }
            if rollback && target >= actual {
                return Err(ModelRegistryError::InvalidRollback { target, actual });
            }
            if definition_digest(&transaction, &provider_id, &model_id, target)?.is_none() {
                return Err(ModelRegistryError::UnknownRevision {
                    identity: identity(&provider_id, &model_id),
                    revision: target,
                });
            }
            let changed = transaction
                .execute(
                    "UPDATE model_registry_heads SET active_revision=?3,updated_at=unixepoch() \
                     WHERE provider_id=?1 AND model_id=?2 AND active_revision=?4",
                    params![
                        provider_id,
                        model_id,
                        sql_index(target)?,
                        sql_index(expected)?
                    ],
                )
                .map_err(storage)?;
            if changed != 1 {
                let actual = active_revision(&transaction, &provider_id, &model_id)?.unwrap_or(0);
                return Err(ModelRegistryError::Conflict {
                    identity: identity(&provider_id, &model_id),
                    expected,
                    actual,
                });
            }
            transaction.commit().map_err(storage)
        })
        .await
}

fn insert_definition(
    connection: &Connection,
    definition: &ModelDefinition,
    json: &str,
    digest: &str,
) -> Result<(), ModelRegistryError> {
    if let Some(existing) = definition_digest(
        connection,
        &definition.provider_id,
        &definition.model_id,
        definition.revision,
    )? {
        return if existing == digest {
            Ok(())
        } else {
            Err(ModelRegistryError::RevisionCollision {
                identity: identity(&definition.provider_id, &definition.model_id),
                revision: definition.revision,
            })
        };
    }
    connection
        .execute(
            "INSERT INTO model_definitions \
             (provider_id,model_id,revision,definition_json,digest,created_at) \
             VALUES (?1,?2,?3,?4,?5,unixepoch())",
            params![
                definition.provider_id,
                definition.model_id,
                sql_index(definition.revision)?,
                json,
                digest
            ],
        )
        .map_err(storage)?;
    Ok(())
}

fn require_provider(connection: &Connection, provider_id: &str) -> Result<(), ModelRegistryError> {
    let exists = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM provider_registry_heads h JOIN provider_definitions d \
             ON d.provider_id=h.provider_id AND d.revision=h.active_revision \
             WHERE h.provider_id=?1)",
            [provider_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(storage)?;
    if exists {
        Ok(())
    } else {
        Err(ModelRegistryError::UnknownProvider(provider_id.to_owned()))
    }
}

fn active_revision(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<Option<u64>, ModelRegistryError> {
    connection
        .query_row(
            "SELECT active_revision FROM model_registry_heads \
             WHERE provider_id=?1 AND model_id=?2",
            params![provider_id, model_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(storage)?
        .map(sql_revision)
        .transpose()
}

fn latest_revision(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<u64, ModelRegistryError> {
    let value = connection
        .query_row(
            "SELECT MAX(revision) FROM model_definitions WHERE provider_id=?1 AND model_id=?2",
            params![provider_id, model_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(storage)?;
    value.map_or(Ok(0), sql_revision)
}

fn definition_digest(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
    revision: u64,
) -> Result<Option<String>, ModelRegistryError> {
    connection
        .query_row(
            "SELECT digest FROM model_definitions \
             WHERE provider_id=?1 AND model_id=?2 AND revision=?3",
            params![provider_id, model_id, sql_index(revision)?],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)
}

fn validate(definition: &ModelDefinition) -> Result<(), ModelRegistryError> {
    definition
        .validate()
        .map_err(|_| ModelRegistryError::InvalidDefinition)
}

fn identity(provider_id: &str, model_id: &str) -> String {
    format!("{provider_id}/{model_id}")
}

fn sql_index(value: u64) -> Result<i64, ModelRegistryError> {
    i64::try_from(value).map_err(|_| ModelRegistryError::InvalidDefinition)
}

fn sql_revision(value: i64) -> Result<u64, ModelRegistryError> {
    u64::try_from(value).map_err(|_| {
        ModelRegistryError::Registry(AgentRegistryError::Integrity(
            "stored Model revision is negative".into(),
        ))
    })
}

fn storage(error: rusqlite::Error) -> ModelRegistryError {
    AgentRegistryError::sqlite(error).into()
}
