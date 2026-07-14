use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::registry_domain::{
    CredentialBindingRevision, CredentialBindingView, StoredRevision, canonical_secret_reference,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum CredentialRegistryError {
    #[error("unknown credential binding `{0}`")]
    UnknownBinding(String),
    #[error("unknown credential generation `{binding_id}`@{generation}")]
    UnknownGeneration { binding_id: String, generation: u64 },
    #[error(
        "credential `{binding_id}` active generation conflict: expected {expected}, found {actual}"
    )]
    Conflict {
        binding_id: String,
        expected: u64,
        actual: u64,
    },
    #[error("credential `{binding_id}` next generation must be {expected}, found {actual}")]
    NonSequential {
        binding_id: String,
        expected: u64,
        actual: u64,
    },
    #[error("credential `{binding_id}` generation {generation} has different content")]
    GenerationCollision { binding_id: String, generation: u64 },
    #[error("credential rollback target {target} is not older than active generation {actual}")]
    InvalidRollback { target: u64, actual: u64 },
    #[error(transparent)]
    Registry(#[from] AgentRegistryError),
}

impl AgentRegistry {
    pub(crate) async fn seed_credential(
        &self,
        definition: CredentialBindingRevision,
    ) -> Result<StoredRevision<CredentialBindingRevision>, CredentialRegistryError> {
        definition.validate()?;
        if definition.generation != 1 {
            return Err(CredentialRegistryError::NonSequential {
                binding_id: definition.binding_id,
                expected: 1,
                actual: definition.generation,
            });
        }
        let binding_id = definition.binding_id.clone();
        let result_id = binding_id.clone();
        let (json, digest) = canonical_secret_reference(&definition.reference)?;
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            if active_generation(&transaction, &binding_id)?.is_none() {
                insert_revision(&transaction, &definition, &json, &digest)?;
                transaction
                    .execute(
                        "INSERT INTO credential_binding_heads \
                         (binding_id,active_generation,updated_at) VALUES (?1,1,?2)",
                        params![binding_id, now()],
                    )
                    .map_err(storage)?;
            }
            transaction.commit().map_err(storage)
        })
        .await?;
        self.load_active_credential(&result_id)
            .await?
            .ok_or(CredentialRegistryError::UnknownBinding(result_id))
    }

    pub(crate) async fn stage_credential(
        &self,
        expected_active: u64,
        definition: CredentialBindingRevision,
    ) -> Result<StoredRevision<CredentialBindingRevision>, CredentialRegistryError> {
        definition.validate()?;
        let binding_id = definition.binding_id.clone();
        let result_id = binding_id.clone();
        let generation = definition.generation;
        let (json, digest) = canonical_secret_reference(&definition.reference)?;
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            require_expected(&transaction, &binding_id, expected_active)?;
            if let Some(existing) = revision_digest(&transaction, &binding_id, generation)? {
                return if existing == digest {
                    transaction.commit().map_err(storage)
                } else {
                    Err(CredentialRegistryError::GenerationCollision {
                        binding_id,
                        generation,
                    })
                };
            }
            let expected = latest_generation(&transaction, &binding_id)? + 1;
            if generation != expected {
                return Err(CredentialRegistryError::NonSequential {
                    binding_id,
                    expected,
                    actual: generation,
                });
            }
            insert_revision(&transaction, &definition, &json, &digest)?;
            transaction.commit().map_err(storage)
        })
        .await?;
        self.load_credential_revision(&result_id, generation)
            .await?
            .ok_or(CredentialRegistryError::UnknownGeneration {
                binding_id: result_id,
                generation,
            })
    }

    pub(crate) async fn activate_credential(
        &self,
        binding_id: &str,
        generation: u64,
        expected_active: u64,
    ) -> Result<(), CredentialRegistryError> {
        set_head(self, binding_id, generation, expected_active, false).await
    }

    pub(crate) async fn rollback_credential(
        &self,
        binding_id: &str,
        target_generation: u64,
        expected_active: u64,
    ) -> Result<(), CredentialRegistryError> {
        set_head(self, binding_id, target_generation, expected_active, true).await
    }

    pub(crate) async fn load_active_credential(
        &self,
        binding_id: &str,
    ) -> Result<Option<StoredRevision<CredentialBindingRevision>>, CredentialRegistryError> {
        let id = binding_id.to_owned();
        let generation = self
            .run_with(move |connection| active_generation(connection, &id))
            .await?;
        match generation {
            Some(generation) => self
                .load_credential_revision(binding_id, generation)
                .await
                .map_err(Into::into),
            None => Ok(None),
        }
    }

    pub(crate) async fn inspect_credentials(
        &self,
        binding_id: &str,
    ) -> Result<Vec<CredentialBindingView>, CredentialRegistryError> {
        let id = binding_id.to_owned();
        let generations: Vec<u64> = self
            .run_with(move |connection| {
                let mut statement = connection
                    .prepare(
                        "SELECT generation FROM credential_binding_revisions \
                         WHERE binding_id=?1 ORDER BY generation DESC",
                    )
                    .map_err(storage)?;
                statement
                    .query_map([id], |row| row.get::<_, i64>(0))
                    .map_err(storage)?
                    .map(|row| row.map_err(storage).and_then(sql_generation))
                    .collect()
            })
            .await?;
        let mut views = Vec::with_capacity(generations.len());
        for generation in generations {
            if let Some(view) = self
                .inspect_credential_revision(binding_id, generation)
                .await?
            {
                views.push(view);
            }
        }
        Ok(views)
    }
}

async fn set_head(
    registry: &AgentRegistry,
    binding_id: &str,
    target: u64,
    expected: u64,
    rollback: bool,
) -> Result<(), CredentialRegistryError> {
    let binding_id = binding_id.to_owned();
    registry
        .run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            let actual = active_generation(&transaction, &binding_id)?
                .ok_or_else(|| CredentialRegistryError::UnknownBinding(binding_id.clone()))?;
            if actual != expected {
                return Err(CredentialRegistryError::Conflict {
                    binding_id,
                    expected,
                    actual,
                });
            }
            if rollback && target >= actual {
                return Err(CredentialRegistryError::InvalidRollback { target, actual });
            }
            if revision_digest(&transaction, &binding_id, target)?.is_none() {
                return Err(CredentialRegistryError::UnknownGeneration {
                    binding_id,
                    generation: target,
                });
            }
            let changed = transaction
                .execute(
                    "UPDATE credential_binding_heads SET active_generation=?2,updated_at=?3 \
                     WHERE binding_id=?1 AND active_generation=?4",
                    params![binding_id, sql_index(target)?, now(), sql_index(expected)?],
                )
                .map_err(storage)?;
            if changed != 1 {
                let actual = active_generation(&transaction, &binding_id)?
                    .ok_or_else(|| CredentialRegistryError::UnknownBinding(binding_id.clone()))?;
                return Err(CredentialRegistryError::Conflict {
                    binding_id,
                    expected,
                    actual,
                });
            }
            transaction.commit().map_err(storage)
        })
        .await
}

fn insert_revision(
    connection: &Connection,
    definition: &CredentialBindingRevision,
    json: &str,
    digest: &str,
) -> Result<(), CredentialRegistryError> {
    connection
        .execute(
            "INSERT INTO credential_binding_revisions \
             (binding_id,generation,reference_json,digest,created_at) VALUES (?1,?2,?3,?4,?5)",
            params![
                definition.binding_id,
                sql_index(definition.generation)?,
                json,
                digest,
                now()
            ],
        )
        .map_err(storage)?;
    Ok(())
}

fn require_expected(
    connection: &Connection,
    binding_id: &str,
    expected: u64,
) -> Result<(), CredentialRegistryError> {
    let actual = active_generation(connection, binding_id)?
        .ok_or_else(|| CredentialRegistryError::UnknownBinding(binding_id.into()))?;
    if actual == expected {
        Ok(())
    } else {
        Err(CredentialRegistryError::Conflict {
            binding_id: binding_id.into(),
            expected,
            actual,
        })
    }
}

fn active_generation(
    connection: &Connection,
    binding_id: &str,
) -> Result<Option<u64>, CredentialRegistryError> {
    connection
        .query_row(
            "SELECT active_generation FROM credential_binding_heads WHERE binding_id=?1",
            [binding_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(storage)?
        .map(sql_generation)
        .transpose()
}

fn latest_generation(
    connection: &Connection,
    binding_id: &str,
) -> Result<u64, CredentialRegistryError> {
    connection
        .query_row(
            "SELECT MAX(generation) FROM credential_binding_revisions WHERE binding_id=?1",
            [binding_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(storage)?
        .map_or(Ok(0), sql_generation)
}

fn revision_digest(
    connection: &Connection,
    binding_id: &str,
    generation: u64,
) -> Result<Option<String>, CredentialRegistryError> {
    connection
        .query_row(
            "SELECT digest FROM credential_binding_revisions \
             WHERE binding_id=?1 AND generation=?2",
            params![binding_id, sql_index(generation)?],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)
}

fn sql_index(value: u64) -> Result<i64, CredentialRegistryError> {
    i64::try_from(value).map_err(|_| {
        AgentRegistryError::Invalid("credential generation exceeds SQLite range".into()).into()
    })
}

fn sql_generation(value: i64) -> Result<u64, CredentialRegistryError> {
    u64::try_from(value)
        .map_err(|_| AgentRegistryError::Integrity("negative credential generation".into()).into())
}

fn storage(error: rusqlite::Error) -> CredentialRegistryError {
    AgentRegistryError::sqlite(error).into()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .cast_signed()
}
