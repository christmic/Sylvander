use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::registry_domain::{ProviderDefinition, StoredRevision, canonical_definition};

const MAX_PROVIDER_PAGE_SIZE: u16 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderRevisionPage {
    pub active_revision: u64,
    pub revisions: Vec<StoredRevision<ProviderDefinition>>,
    pub next_before_revision: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderRegistryError {
    #[error("invalid Provider definition")]
    InvalidDefinition,
    #[error("Provider `{provider_id}` already exists")]
    AlreadyExists { provider_id: String },
    #[error("unknown Provider `{0}`")]
    UnknownProvider(String),
    #[error("unknown Provider revision `{provider_id}`@{revision}")]
    UnknownRevision { provider_id: String, revision: u64 },
    #[error(
        "Provider `{provider_id}` active revision conflict: expected {expected}, found {actual}"
    )]
    Conflict {
        provider_id: String,
        expected: u64,
        actual: u64,
    },
    #[error("Provider `{provider_id}` next revision must be {expected}, found {actual}")]
    NonSequential {
        provider_id: String,
        expected: u64,
        actual: u64,
    },
    #[error("Provider `{provider_id}` revision {revision} has different content")]
    RevisionCollision { provider_id: String, revision: u64 },
    #[error("Provider rollback target {target} is not older than active revision {actual}")]
    InvalidRollback { target: u64, actual: u64 },
    #[error(transparent)]
    Registry(#[from] AgentRegistryError),
}

impl AgentRegistry {
    /// Create a new Provider at revision one without bootstrap idempotency.
    pub(crate) async fn create_provider(
        &self,
        definition: ProviderDefinition,
    ) -> Result<StoredRevision<ProviderDefinition>, ProviderRegistryError> {
        validate(&definition)?;
        if definition.revision != 1 {
            return Err(ProviderRegistryError::NonSequential {
                provider_id: definition.id,
                expected: 1,
                actual: definition.revision,
            });
        }
        let provider_id = definition.id.clone();
        let result_provider_id = provider_id.clone();
        let (json, digest) = canonical_definition(&definition)?;
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            if provider_exists(&transaction, &provider_id)? {
                return Err(ProviderRegistryError::AlreadyExists { provider_id });
            }
            insert_definition(&transaction, &definition, &json, &digest)?;
            transaction
                .execute(
                    "INSERT INTO provider_registry_heads(provider_id,active_revision,updated_at) \
                     VALUES (?1,1,?2)",
                    params![provider_id, now()],
                )
                .map_err(storage)?;
            transaction.commit().map_err(storage)
        })
        .await?;
        self.load_active_provider(&result_provider_id)
            .await?
            .ok_or_else(|| {
                AgentRegistryError::Integrity("created Provider disappeared".into()).into()
            })
    }

    pub(crate) async fn seed_provider(
        &self,
        definition: ProviderDefinition,
    ) -> Result<StoredRevision<ProviderDefinition>, ProviderRegistryError> {
        validate(&definition)?;
        if definition.revision != 1 {
            return Err(ProviderRegistryError::NonSequential {
                provider_id: definition.id,
                expected: 1,
                actual: definition.revision,
            });
        }
        let provider_id = definition.id.clone();
        let result_provider_id = provider_id.clone();
        let (json, digest) = canonical_definition(&definition)?;
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            if active_revision(&transaction, &provider_id)?.is_some() {
                transaction.commit().map_err(storage)?;
                return Ok(());
            }
            insert_definition(&transaction, &definition, &json, &digest)?;
            transaction
                .execute(
                    "INSERT INTO provider_registry_heads(provider_id,active_revision,updated_at) \
                     VALUES (?1,1,?2)",
                    params![provider_id, now()],
                )
                .map_err(storage)?;
            transaction.commit().map_err(storage)
        })
        .await?;
        self.load_active_provider(&result_provider_id)
            .await?
            .ok_or(ProviderRegistryError::UnknownProvider(result_provider_id))
    }

    pub(crate) async fn stage_provider(
        &self,
        expected_active: u64,
        definition: ProviderDefinition,
    ) -> Result<StoredRevision<ProviderDefinition>, ProviderRegistryError> {
        validate(&definition)?;
        let provider_id = definition.id.clone();
        let result_provider_id = provider_id.clone();
        let revision = definition.revision;
        let (json, digest) = canonical_definition(&definition)?;
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            require_expected(&transaction, &provider_id, expected_active)?;
            if let Some(existing) = definition_digest(&transaction, &provider_id, revision)? {
                return if existing == digest {
                    transaction.commit().map_err(storage)
                } else {
                    Err(ProviderRegistryError::RevisionCollision {
                        provider_id,
                        revision,
                    })
                };
            }
            let latest = latest_revision(&transaction, &provider_id)?;
            if revision != latest + 1 {
                return Err(ProviderRegistryError::NonSequential {
                    provider_id,
                    expected: latest + 1,
                    actual: revision,
                });
            }
            insert_definition(&transaction, &definition, &json, &digest)?;
            transaction.commit().map_err(storage)
        })
        .await?;
        self.load_provider_revision(&result_provider_id, revision)
            .await?
            .ok_or(ProviderRegistryError::UnknownRevision {
                provider_id: result_provider_id,
                revision,
            })
    }

    pub(crate) async fn activate_provider(
        &self,
        provider_id: &str,
        revision: u64,
        expected_active: u64,
    ) -> Result<(), ProviderRegistryError> {
        set_head(self, provider_id, revision, expected_active, false).await
    }

    pub(crate) async fn rollback_provider(
        &self,
        provider_id: &str,
        target_revision: u64,
        expected_active: u64,
    ) -> Result<(), ProviderRegistryError> {
        set_head(self, provider_id, target_revision, expected_active, true).await
    }

    pub(crate) async fn load_active_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<StoredRevision<ProviderDefinition>>, ProviderRegistryError> {
        let id = provider_id.to_owned();
        let revision = self
            .run_with(move |connection| active_revision(connection, &id))
            .await?;
        match revision {
            Some(revision) => self
                .load_provider_revision(provider_id, revision)
                .await
                .map_err(Into::into),
            None => Ok(None),
        }
    }

    pub(crate) async fn inspect_provider(
        &self,
        provider_id: &str,
    ) -> Result<Vec<StoredRevision<ProviderDefinition>>, ProviderRegistryError> {
        let id = provider_id.to_owned();
        let revisions: Vec<u64> = self
            .run_with(move |connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT revision FROM provider_definitions \
                         WHERE provider_id=?1 ORDER BY revision DESC",
                    )
                    .map_err(storage)?;
                statement
                    .query_map([id], |row| row.get::<_, i64>(0))
                    .map_err(storage)?
                    .map(|row| row.map_err(storage).and_then(sql_revision))
                    .collect()
            })
            .await?;
        let mut stored = Vec::with_capacity(revisions.len());
        for revision in revisions {
            if let Some(item) = self.load_provider_revision(provider_id, revision).await? {
                stored.push(item);
            }
        }
        Ok(stored)
    }

    /// Load one bounded Provider revision page in a single database query.
    pub(crate) async fn inspect_provider_page(
        &self,
        provider_id: &str,
        before_revision: Option<u64>,
        limit: u16,
    ) -> Result<ProviderRevisionPage, ProviderRegistryError> {
        if !(1..=MAX_PROVIDER_PAGE_SIZE).contains(&limit) || before_revision == Some(0) {
            return Err(
                AgentRegistryError::Invalid("Provider page bounds are invalid".into()).into(),
            );
        }
        let provider_id = provider_id.to_owned();
        let before = before_revision.map(sql_index).transpose()?;
        let sql_limit = i64::from(limit) + 1;
        self.run_with(move |connection| {
            let mut statement = connection
                .prepare(
                    "WITH state AS (
                       SELECT
                         (SELECT active_revision FROM provider_registry_heads WHERE provider_id=?1) AS active_revision,
                         EXISTS(SELECT 1 FROM provider_definitions WHERE provider_id=?1) AS has_revisions,
                         EXISTS(
                           SELECT 1 FROM provider_registry_heads h
                           JOIN provider_definitions d
                             ON d.provider_id=h.provider_id AND d.revision=h.active_revision
                           WHERE h.provider_id=?1
                         ) AS active_exists
                     )
                     SELECT state.active_revision, state.has_revisions, state.active_exists,
                            d.revision, d.definition_json, d.digest, d.created_at
                     FROM state
                     LEFT JOIN provider_definitions d
                       ON d.provider_id=?1 AND (?2 IS NULL OR d.revision < ?2)
                     ORDER BY d.revision DESC
                     LIMIT ?3",
                )
                .map_err(storage)?;
            let mut rows = statement
                .query(params![provider_id, before, sql_limit])
                .map_err(storage)?;
            let mut active_revision = None;
            let mut revisions = Vec::with_capacity(usize::from(limit) + 1);
            while let Some(row) = rows.next().map_err(storage)? {
                let active = row.get::<_, Option<i64>>(0).map_err(storage)?;
                let has_revisions = row.get::<_, bool>(1).map_err(storage)?;
                let active_exists = row.get::<_, bool>(2).map_err(storage)?;
                let Some(active) = active else {
                    return if has_revisions {
                        Err(AgentRegistryError::Integrity(
                            "Provider registry head is missing".into(),
                        )
                        .into())
                    } else {
                        Err(ProviderRegistryError::UnknownProvider(provider_id))
                    };
                };
                if !active_exists {
                    return Err(AgentRegistryError::Integrity(
                        "Provider active revision is missing".into(),
                    )
                    .into());
                }
                let active = sql_revision(active)?;
                active_revision = Some(active);
                let Some(revision) = row.get::<_, Option<i64>>(3).map_err(storage)? else {
                    continue;
                };
                revisions.push(decode_revision(
                    &provider_id,
                    revision,
                    required_column(row.get(4).map_err(storage)?)?,
                    required_column(row.get(5).map_err(storage)?)?,
                    required_column(row.get(6).map_err(storage)?)?,
                    active,
                )?);
            }
            let active_revision = active_revision
                .ok_or_else(|| ProviderRegistryError::UnknownProvider(provider_id.clone()))?;
            let next_before_revision = (revisions.len() > usize::from(limit))
                .then(|| revisions[usize::from(limit) - 1].definition.revision);
            revisions.truncate(usize::from(limit));
            Ok(ProviderRevisionPage {
                active_revision,
                revisions,
                next_before_revision,
            })
        })
        .await
    }
}

fn required_column<T>(value: Option<T>) -> Result<T, ProviderRegistryError> {
    value.ok_or_else(|| {
        AgentRegistryError::Integrity("Provider revision row is incomplete".into()).into()
    })
}

fn decode_revision(
    provider_id: &str,
    revision: i64,
    definition_json: String,
    stored_digest: String,
    created_at: i64,
    active_revision: u64,
) -> Result<StoredRevision<ProviderDefinition>, ProviderRegistryError> {
    let revision = sql_revision(revision)?;
    let definition: ProviderDefinition =
        serde_json::from_str(&definition_json).map_err(AgentRegistryError::serde)?;
    validate(&definition)?;
    if definition.id != provider_id || definition.revision != revision {
        return Err(AgentRegistryError::Integrity("Provider identity mismatch".into()).into());
    }
    let (expected_json, expected_digest) = canonical_definition(&definition)?;
    if definition_json != expected_json || stored_digest != expected_digest {
        return Err(
            AgentRegistryError::Integrity("Provider revision digest mismatch".into()).into(),
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
    provider_id: &str,
    target: u64,
    expected: u64,
    rollback: bool,
) -> Result<(), ProviderRegistryError> {
    let provider_id = provider_id.to_owned();
    registry
        .run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            let actual = active_revision(&transaction, &provider_id)?
                .ok_or_else(|| ProviderRegistryError::UnknownProvider(provider_id.clone()))?;
            if actual != expected {
                return Err(ProviderRegistryError::Conflict {
                    provider_id,
                    expected,
                    actual,
                });
            }
            if rollback && target >= actual {
                return Err(ProviderRegistryError::InvalidRollback { target, actual });
            }
            if definition_digest(&transaction, &provider_id, target)?.is_none() {
                return Err(ProviderRegistryError::UnknownRevision {
                    provider_id,
                    revision: target,
                });
            }
            let changed = transaction
                .execute(
                    "UPDATE provider_registry_heads SET active_revision=?2,updated_at=?3 \
                     WHERE provider_id=?1 AND active_revision=?4",
                    params![provider_id, sql_index(target)?, now(), sql_index(expected)?],
                )
                .map_err(storage)?;
            if changed != 1 {
                let actual = active_revision(&transaction, &provider_id)?
                    .ok_or_else(|| ProviderRegistryError::UnknownProvider(provider_id.clone()))?;
                return Err(ProviderRegistryError::Conflict {
                    provider_id,
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
    definition: &ProviderDefinition,
    json: &str,
    digest: &str,
) -> Result<(), ProviderRegistryError> {
    if let Some(existing) = definition_digest(connection, &definition.id, definition.revision)? {
        return if existing == digest {
            Ok(())
        } else {
            Err(ProviderRegistryError::RevisionCollision {
                provider_id: definition.id.clone(),
                revision: definition.revision,
            })
        };
    }
    connection
        .execute(
            "INSERT INTO provider_definitions \
             (provider_id,revision,definition_json,digest,credential_binding_id,created_at) \
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                definition.id,
                sql_index(definition.revision)?,
                json,
                digest,
                definition.credential_binding_id,
                now()
            ],
        )
        .map_err(storage)?;
    Ok(())
}

fn require_expected(
    connection: &Connection,
    provider_id: &str,
    expected: u64,
) -> Result<(), ProviderRegistryError> {
    let actual = active_revision(connection, provider_id)?
        .ok_or_else(|| ProviderRegistryError::UnknownProvider(provider_id.to_owned()))?;
    if actual == expected {
        Ok(())
    } else {
        Err(ProviderRegistryError::Conflict {
            provider_id: provider_id.to_owned(),
            expected,
            actual,
        })
    }
}

fn active_revision(
    connection: &Connection,
    provider_id: &str,
) -> Result<Option<u64>, ProviderRegistryError> {
    connection
        .query_row(
            "SELECT active_revision FROM provider_registry_heads WHERE provider_id=?1",
            [provider_id],
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
) -> Result<u64, ProviderRegistryError> {
    let value = connection
        .query_row(
            "SELECT MAX(revision) FROM provider_definitions WHERE provider_id=?1",
            [provider_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(storage)?;
    value.map_or(Ok(0), sql_revision)
}

fn provider_exists(
    connection: &Connection,
    provider_id: &str,
) -> Result<bool, ProviderRegistryError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM provider_registry_heads WHERE provider_id=?1) \
                    OR EXISTS(SELECT 1 FROM provider_definitions WHERE provider_id=?1)",
            [provider_id],
            |row| row.get(0),
        )
        .map_err(storage)
}

fn definition_digest(
    connection: &Connection,
    provider_id: &str,
    revision: u64,
) -> Result<Option<String>, ProviderRegistryError> {
    connection
        .query_row(
            "SELECT digest FROM provider_definitions WHERE provider_id=?1 AND revision=?2",
            params![provider_id, sql_index(revision)?],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)
}

fn validate(definition: &ProviderDefinition) -> Result<(), ProviderRegistryError> {
    definition
        .validate()
        .map_err(|_| ProviderRegistryError::InvalidDefinition)
}

fn sql_index(value: u64) -> Result<i64, ProviderRegistryError> {
    i64::try_from(value).map_err(|_| ProviderRegistryError::InvalidDefinition)
}

fn sql_revision(value: i64) -> Result<u64, ProviderRegistryError> {
    u64::try_from(value).map_err(|_| {
        ProviderRegistryError::Registry(AgentRegistryError::Integrity(
            "stored Provider revision is negative".into(),
        ))
    })
}

fn storage(error: rusqlite::Error) -> ProviderRegistryError {
    AgentRegistryError::sqlite(error).into()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .cast_signed()
}
