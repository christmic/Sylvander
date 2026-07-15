use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::config::{SecretRef, SecretResolver, SecretValue};
use crate::registry_domain::{
    CredentialBindingRevision, CredentialBindingView, SecretReferenceKind, StoredRevision,
    canonical_secret_reference,
};

const MAX_CREDENTIAL_PAGE_SIZE: u16 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CredentialBindingPage {
    pub active_generation: u64,
    pub generations: Vec<CredentialBindingView>,
    pub next_before_generation: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CredentialRegistryError {
    #[error("unknown credential binding `{0}`")]
    UnknownBinding(String),
    #[error("credential binding `{binding_id}` already exists with different content")]
    AlreadyExists { binding_id: String },
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
    #[error("credential `{binding_id}` generation {generation} could not be resolved")]
    Resolution { binding_id: String, generation: u64 },
    #[error(transparent)]
    Registry(#[from] AgentRegistryError),
}

/// Request-scoped secret material. It is deliberately neither cloneable nor
/// serializable, and its formatting never exposes metadata or bytes.
pub(crate) struct ResolvedCredential {
    generation: u64,
    value: SecretValue,
}

impl ResolvedCredential {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn value(&self) -> &SecretValue {
        &self.value
    }
}

impl std::fmt::Debug for ResolvedCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResolvedCredential([REDACTED])")
    }
}

pub(crate) trait CredentialSecretResolver: Send + Sync {
    fn resolve_credential(&self, reference: &crate::config::SecretRef) -> Result<SecretValue, ()>;
}

impl<T: SecretResolver + ?Sized> CredentialSecretResolver for T {
    fn resolve_credential(&self, reference: &crate::config::SecretRef) -> Result<SecretValue, ()> {
        self.resolve(reference).map_err(|_| ())
    }
}

impl AgentRegistry {
    /// Create a new binding at generation one without bootstrap's overwrite-ignore semantics.
    pub(crate) async fn create_credential_binding(
        &self,
        binding_id: &str,
        reference: SecretRef,
    ) -> Result<StoredRevision<CredentialBindingRevision>, CredentialRegistryError> {
        let definition = CredentialBindingRevision {
            binding_id: binding_id.to_owned(),
            generation: 1,
            reference,
        };
        definition.validate()?;
        let result_id = definition.binding_id.clone();
        let (json, digest) = canonical_secret_reference(&definition.reference)?;
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            let active = active_generation(&transaction, &definition.binding_id)?;
            let latest = latest_generation(&transaction, &definition.binding_id)?;
            let existing: Option<(String, String)> = transaction
                .query_row(
                    "SELECT reference_json,digest FROM credential_binding_revisions \
                     WHERE binding_id=?1 AND generation=1",
                    [&definition.binding_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(storage)?;
            match (active, latest, existing) {
                (None, 0, None) => {
                    insert_revision(&transaction, &definition, &json, &digest)?;
                    transaction
                        .execute(
                            "INSERT INTO credential_binding_heads \
                             (binding_id,active_generation,updated_at) VALUES (?1,1,?2)",
                            params![definition.binding_id, now()],
                        )
                        .map_err(storage)?;
                }
                (Some(active), _, Some((stored_json, stored_digest))) => {
                    if revision_digest(&transaction, &definition.binding_id, active)?.is_none() {
                        return Err(AgentRegistryError::Integrity(
                            "credential active generation is missing".into(),
                        )
                        .into());
                    }
                    let stored_reference: SecretRef =
                        serde_json::from_str(&stored_json).map_err(|_| {
                            AgentRegistryError::Integrity("credential reference is invalid".into())
                        })?;
                    let (expected_json, expected_digest) =
                        canonical_secret_reference(&stored_reference)?;
                    if stored_json != expected_json || stored_digest != expected_digest {
                        return Err(AgentRegistryError::Integrity(
                            "credential reference digest mismatch".into(),
                        )
                        .into());
                    }
                    if stored_digest != digest {
                        return Err(CredentialRegistryError::AlreadyExists {
                            binding_id: definition.binding_id,
                        });
                    }
                }
                (None, _, Some(_)) => {
                    return Err(AgentRegistryError::Integrity(
                        "credential binding head is missing".into(),
                    )
                    .into());
                }
                _ => {
                    return Err(AgentRegistryError::Integrity(
                        "credential generation one is missing".into(),
                    )
                    .into());
                }
            }
            transaction.commit().map_err(storage)
        })
        .await?;
        self.load_credential_revision(&result_id, 1)
            .await?
            .ok_or_else(|| {
                AgentRegistryError::Integrity("credential generation one disappeared".into()).into()
            })
    }

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

    /// Resolve the active reference for one request. No result or failure is
    /// cached, and resolver details are intentionally erased from the error.
    pub(crate) async fn resolve_active_credential<R: CredentialSecretResolver + ?Sized>(
        &self,
        binding_id: &str,
        resolver: &R,
    ) -> Result<ResolvedCredential, CredentialRegistryError> {
        let stored = self
            .load_active_credential(binding_id)
            .await?
            .ok_or_else(|| CredentialRegistryError::UnknownBinding(binding_id.into()))?;
        let generation = stored.definition.generation;
        let value = resolver
            .resolve_credential(&stored.definition.reference)
            .map_err(|()| CredentialRegistryError::Resolution {
                binding_id: binding_id.into(),
                generation,
            })?;
        Ok(ResolvedCredential { generation, value })
    }

    /// Resolve one immutable generation only long enough to prove availability.
    pub(crate) async fn preflight_credential_generation<R: CredentialSecretResolver + ?Sized>(
        &self,
        binding_id: &str,
        generation: u64,
        resolver: &R,
    ) -> Result<u64, CredentialRegistryError> {
        let stored = self
            .load_credential_revision(binding_id, generation)
            .await?
            .ok_or_else(|| CredentialRegistryError::UnknownGeneration {
                binding_id: binding_id.into(),
                generation,
            })?;
        let secret = resolver
            .resolve_credential(&stored.definition.reference)
            .map_err(|()| CredentialRegistryError::Resolution {
                binding_id: binding_id.into(),
                generation,
            })?;
        secret
            .as_str()
            .map_err(|_| CredentialRegistryError::Resolution {
                binding_id: binding_id.into(),
                generation,
            })?;
        drop(secret);
        Ok(generation)
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

    /// Inspect one bounded generation page without resolving secret material.
    pub(crate) async fn inspect_credential_page(
        &self,
        binding_id: &str,
        before_generation: Option<u64>,
        limit: u16,
    ) -> Result<CredentialBindingPage, CredentialRegistryError> {
        if !(1..=MAX_CREDENTIAL_PAGE_SIZE).contains(&limit) || before_generation == Some(0) {
            return Err(
                AgentRegistryError::Invalid("credential page bounds are invalid".into()).into(),
            );
        }
        let binding_id = binding_id.to_owned();
        let before = before_generation.map(sql_index).transpose()?;
        let sql_limit = i64::from(limit) + 1;
        self.run_with(move |connection| {
            let mut statement = connection
                .prepare(
                    "WITH state AS (
                       SELECT
                         (SELECT active_generation FROM credential_binding_heads WHERE binding_id=?1) AS active_generation,
                         EXISTS(SELECT 1 FROM credential_binding_revisions WHERE binding_id=?1) AS has_revisions,
                         EXISTS(
                           SELECT 1 FROM credential_binding_heads h
                           JOIN credential_binding_revisions d
                             ON d.binding_id=h.binding_id AND d.generation=h.active_generation
                           WHERE h.binding_id=?1
                         ) AS active_exists
                     )
                     SELECT state.active_generation, state.has_revisions, state.active_exists,
                            d.generation, d.reference_json, d.digest, d.created_at
                     FROM state
                     LEFT JOIN credential_binding_revisions d
                       ON d.binding_id=?1 AND (?2 IS NULL OR d.generation < ?2)
                     ORDER BY d.generation DESC
                     LIMIT ?3",
                )
                .map_err(storage)?;
            let mut rows = statement
                .query(params![binding_id, before, sql_limit])
                .map_err(storage)?;
            let mut active_generation = None;
            let mut views = Vec::with_capacity(usize::from(limit) + 1);
            while let Some(row) = rows.next().map_err(storage)? {
                let active = row.get::<_, Option<i64>>(0).map_err(storage)?;
                let has_revisions = row.get::<_, bool>(1).map_err(storage)?;
                let active_exists = row.get::<_, bool>(2).map_err(storage)?;
                let Some(active) = active else {
                    return if has_revisions {
                        Err(AgentRegistryError::Integrity(
                            "credential binding head is missing".into(),
                        )
                        .into())
                    } else {
                        Err(CredentialRegistryError::UnknownBinding(binding_id))
                    };
                };
                if !active_exists {
                    return Err(AgentRegistryError::Integrity(
                        "credential active generation is missing".into(),
                    )
                    .into());
                }
                let active = sql_generation(active)?;
                active_generation = Some(active);
                let Some(generation) = row.get::<_, Option<i64>>(3).map_err(storage)? else {
                    continue;
                };
                let reference_json = required_column(row.get(4).map_err(storage)?)?;
                let digest = required_column(row.get(5).map_err(storage)?)?;
                let created_at = required_column(row.get(6).map_err(storage)?)?;
                views.push(decode_view(
                    &binding_id,
                    generation,
                    reference_json,
                    digest,
                    created_at,
                    active,
                )?);
            }
            let active_generation = active_generation.ok_or_else(|| {
                CredentialRegistryError::UnknownBinding(binding_id.clone())
            })?;
            let next_before_generation = (views.len() > usize::from(limit))
                .then(|| views[usize::from(limit) - 1].generation);
            views.truncate(usize::from(limit));
            Ok(CredentialBindingPage {
                active_generation,
                generations: views,
                next_before_generation,
            })
        })
        .await
    }
}

fn required_column<T>(value: Option<T>) -> Result<T, CredentialRegistryError> {
    value.ok_or_else(|| {
        AgentRegistryError::Integrity("credential revision row is incomplete".into()).into()
    })
}

fn decode_view(
    binding_id: &str,
    generation: i64,
    reference_json: String,
    stored_digest: String,
    created_at: i64,
    active_generation: u64,
) -> Result<CredentialBindingView, CredentialRegistryError> {
    let generation = sql_generation(generation)?;
    let reference: SecretRef =
        serde_json::from_str(&reference_json).map_err(AgentRegistryError::serde)?;
    let definition = CredentialBindingRevision {
        binding_id: binding_id.into(),
        generation,
        reference,
    };
    definition.validate()?;
    let (_, expected_digest) = canonical_secret_reference(&definition.reference)?;
    if stored_digest != expected_digest {
        return Err(
            AgentRegistryError::Integrity("credential reference digest mismatch".into()).into(),
        );
    }
    Ok(CredentialBindingView {
        binding_id: definition.binding_id,
        generation,
        reference_kind: match definition.reference {
            SecretRef::Env { .. } => SecretReferenceKind::Environment,
            SecretRef::File { .. } => SecretReferenceKind::File,
        },
        reference_configured: true,
        reference_digest_sha256: stored_digest,
        created_at,
        active: generation == active_generation,
    })
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
