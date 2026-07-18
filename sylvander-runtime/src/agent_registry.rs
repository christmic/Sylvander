//! Durable, immutable Agent definitions and explicit revision activation.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use sylvander_protocol::AgentId;
use tokio::sync::Mutex;
use tokio::task;

use crate::agent_registry_snapshot_v3::{
    AgentRegistrySnapshotV3, AgentSnapshotSelectionV3, AgentSnapshotV3Error,
    stage_snapshot_v3_in_transaction,
};
use crate::config::{AgentDefinitionConfig, ServerConfig};

#[derive(Clone)]
pub struct AgentRegistry {
    connection: Arc<Mutex<Connection>>,
    allowed_foreign_objects: Arc<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct AgentRevision {
    pub definition: AgentDefinitionConfig,
    pub digest: String,
    pub created_at: i64,
    pub active: bool,
}

impl AgentRegistry {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, AgentRegistryError> {
        let path = path.as_ref().to_path_buf();
        Self::open_connection(move || Connection::open(path), Vec::new()).await
    }

    /// Open a registry in a database shared with another component.
    ///
    /// The caller supplies the other component's complete current object-name
    /// allowlist. Registry-owned SQL remains exact-match validated and any
    /// object outside the two declared namespaces fails closed.
    pub async fn open_shared(
        path: impl AsRef<Path>,
        allowed_foreign_objects: &[&str],
    ) -> Result<Self, AgentRegistryError> {
        let path = path.as_ref().to_path_buf();
        Self::open_connection(
            move || Connection::open(path),
            allowed_foreign_objects
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
        )
        .await
    }

    #[cfg(test)]
    async fn open_in_memory() -> Result<Self, AgentRegistryError> {
        Self::open_connection(Connection::open_in_memory, Vec::new()).await
    }

    async fn open_connection(
        open: impl FnOnce() -> rusqlite::Result<Connection> + Send + 'static,
        allowed_foreign_objects: Vec<String>,
    ) -> Result<Self, AgentRegistryError> {
        task::spawn_blocking(move || {
            let mut connection = open().map_err(AgentRegistryError::sqlite)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(AgentRegistryError::sqlite)?;
            connection
                .execute_batch("PRAGMA foreign_keys=ON;")
                .map_err(AgentRegistryError::sqlite)?;
            initialize_or_validate_registry_schema(&mut connection, &allowed_foreign_objects)?;
            Ok(Self {
                connection: Arc::new(Mutex::new(connection)),
                allowed_foreign_objects: Arc::new(allowed_foreign_objects),
            })
        })
        .await
        .map_err(|error| AgentRegistryError::Task(error.to_string()))?
    }

    pub(crate) async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, AgentRegistryError> + Send + 'static,
    ) -> Result<T, AgentRegistryError> {
        let connection = self.connection.clone();
        let allowed_foreign_objects = self.allowed_foreign_objects.clone();
        task::spawn_blocking(move || {
            let mut connection = connection.blocking_lock();
            validate_registry_object_namespace(
                &registry_schema_objects(&connection)?,
                &allowed_foreign_objects,
            )?;
            operation(&mut connection)
        })
        .await
        .map_err(|error| AgentRegistryError::Task(error.to_string()))?
    }

    pub(crate) async fn run_with<T, E>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<AgentRegistryError> + Send + 'static,
    {
        let connection = self.connection.clone();
        let allowed_foreign_objects = self.allowed_foreign_objects.clone();
        task::spawn_blocking(move || {
            let mut connection = connection.blocking_lock();
            validate_registry_object_namespace(
                &registry_schema_objects(&connection).map_err(E::from)?,
                &allowed_foreign_objects,
            )
            .map_err(E::from)?;
            operation(&mut connection)
        })
        .await
        .map_err(|error| E::from(AgentRegistryError::Task(error.to_string())))?
    }

    /// Import validated configuration definitions without changing an
    /// existing active revision. Configuration is bootstrap input, not an
    /// implicit rollback mechanism.
    pub async fn seed(&self, config: &ServerConfig) -> Result<(), AgentRegistryError> {
        config
            .validate()
            .map_err(|error| AgentRegistryError::Invalid(error.to_string()))?;
        let definitions = config.agents.clone();
        self.run(move |connection| {
            let transaction = connection
                .transaction()
                .map_err(AgentRegistryError::sqlite)?;
            for definition in definitions {
                insert_definition(&transaction, &definition, true)?;
            }
            transaction.commit().map_err(AgentRegistryError::sqlite)
        })
        .await
    }

    /// Validate and append the next immutable revision. Activation is a
    /// separate operation so a staged definition cannot affect sessions.
    pub async fn update(
        &self,
        catalog: &ServerConfig,
        expected_active: u64,
        definition: AgentDefinitionConfig,
    ) -> Result<AgentRevision, AgentRegistryError> {
        validate_candidate(catalog, &definition)?;
        let agent_id = definition.spec.id.0.clone();
        self.run(move |connection| {
            let transaction = connection
                .transaction()
                .map_err(AgentRegistryError::sqlite)?;
            let active = active_revision(&transaction, &agent_id)?
                .ok_or_else(|| AgentRegistryError::UnknownAgent(agent_id.clone()))?;
            if active != expected_active {
                return Err(AgentRegistryError::Conflict {
                    agent_id,
                    expected: expected_active,
                    actual: active,
                });
            }
            let latest: u64 = transaction
                .query_row(
                    "SELECT MAX(revision) FROM agent_definitions WHERE agent_id=?1",
                    [&definition.spec.id.0],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .map_err(AgentRegistryError::sqlite)?
                .map_or(Ok(0), decode_revision)?;
            if definition.revision != latest + 1 {
                return Err(AgentRegistryError::NonSequential {
                    agent_id: definition.spec.id.0.clone(),
                    expected: latest + 1,
                    actual: definition.revision,
                });
            }
            let revision = definition.revision;
            insert_definition(&transaction, &definition, false)?;
            let stored = load_revision(&transaction, &agent_id, revision)?.ok_or_else(|| {
                AgentRegistryError::Integrity(format!(
                    "newly inserted Agent revision `{agent_id}`@{revision} is missing"
                ))
            })?;
            transaction.commit().map_err(AgentRegistryError::sqlite)?;
            Ok(stored)
        })
        .await
    }

    /// Atomically append one Agent revision and its exact V3 registry pins.
    pub(crate) async fn stage_agent_revision_v3(
        &self,
        expected_active: u64,
        definition: AgentDefinitionConfig,
        selection: AgentSnapshotSelectionV3,
    ) -> Result<(AgentRevision, AgentRegistrySnapshotV3), AgentSnapshotV3Error> {
        selection.validate()?;
        let configured_default = sylvander_protocol::ModelSelection {
            provider_id: definition.spec.model.provider.clone(),
            model_id: definition.spec.model.model_name.clone(),
        };
        if selection.agent_id != definition.spec.id.0
            || selection.agent_revision != definition.revision
            || selection.default_model != configured_default
            || (!definition.spec.model.allowed_models.is_empty()
                && selection.allowed_models
                    != definition
                        .spec
                        .model
                        .allowed_models
                        .iter()
                        .cloned()
                        .collect())
        {
            return Err(AgentSnapshotV3Error::DefinitionSelectionMismatch);
        }

        let agent_id = definition.spec.id.0.clone();
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(AgentRegistryError::sqlite)?;
            let active = active_revision(&transaction, &agent_id)?
                .ok_or_else(|| AgentRegistryError::UnknownAgent(agent_id.clone()))?;
            if active != expected_active {
                return Err(AgentRegistryError::Conflict {
                    agent_id,
                    expected: expected_active,
                    actual: active,
                }
                .into());
            }
            let latest = transaction
                .query_row(
                    "SELECT MAX(revision) FROM agent_definitions WHERE agent_id=?1",
                    [&definition.spec.id.0],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .map_err(AgentRegistryError::sqlite)?
                .map_or(Ok(0), decode_revision)?;
            if definition.revision != latest + 1 {
                return Err(AgentRegistryError::NonSequential {
                    agent_id: definition.spec.id.0.clone(),
                    expected: latest + 1,
                    actual: definition.revision,
                }
                .into());
            }

            let revision = definition.revision;
            insert_definition(&transaction, &definition, false)?;
            let stored = load_revision(&transaction, &agent_id, revision)?.ok_or_else(|| {
                AgentRegistryError::Integrity(format!(
                    "newly inserted Agent revision `{agent_id}`@{revision} is missing"
                ))
            })?;
            let snapshot = stage_snapshot_v3_in_transaction(&transaction, selection)?;
            transaction.commit().map_err(AgentRegistryError::sqlite)?;
            Ok((stored, snapshot))
        })
        .await
    }

    pub async fn activate(
        &self,
        agent_id: &AgentId,
        revision: u64,
        expected_active: u64,
    ) -> Result<(), AgentRegistryError> {
        let agent_id = agent_id.0.clone();
        self.run(move |connection| {
            set_active(connection, &agent_id, revision, expected_active, false)
        })
        .await
    }

    pub async fn rollback(
        &self,
        agent_id: &AgentId,
        target_revision: u64,
        expected_active: u64,
    ) -> Result<(), AgentRegistryError> {
        let agent_id = agent_id.0.clone();
        self.run(move |connection| {
            set_active(
                connection,
                &agent_id,
                target_revision,
                expected_active,
                true,
            )
        })
        .await
    }

    pub async fn load(
        &self,
        agent_id: &AgentId,
        revision: u64,
    ) -> Result<Option<AgentRevision>, AgentRegistryError> {
        let agent_id = agent_id.0.clone();
        self.run(move |connection| load_revision(connection, &agent_id, revision))
            .await
    }

    pub async fn load_active(
        &self,
        agent_id: &AgentId,
    ) -> Result<Option<AgentRevision>, AgentRegistryError> {
        let agent_id = agent_id.0.clone();
        self.run(move |connection| {
            let Some(revision) = active_revision(connection, &agent_id)? else {
                return Ok(None);
            };
            load_revision(connection, &agent_id, revision)
        })
        .await
    }

    pub async fn inspect(
        &self,
        agent_id: &AgentId,
    ) -> Result<Vec<AgentRevision>, AgentRegistryError> {
        let agent_id = agent_id.0.clone();
        self.run(move |connection| {
            let active = active_revision(connection, &agent_id)?;
            let mut statement = connection
                .prepare(
                    "SELECT definition_json, digest, created_at, revision \
                     FROM agent_definitions WHERE agent_id=?1 ORDER BY revision DESC",
                )
                .map_err(AgentRegistryError::sqlite)?;
            let rows = statement
                .query_map([&agent_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .map_err(AgentRegistryError::sqlite)?;
            rows.map(|row| {
                let (json, digest, created_at, revision) =
                    row.map_err(AgentRegistryError::sqlite)?;
                let revision = decode_revision(revision)?;
                decode_stored_revision(
                    &agent_id,
                    revision,
                    json,
                    digest,
                    created_at,
                    active == Some(revision),
                )
            })
            .collect()
        })
        .await
    }
}

fn validate_candidate(
    catalog: &ServerConfig,
    definition: &AgentDefinitionConfig,
) -> Result<(), AgentRegistryError> {
    let mut candidate = catalog.clone();
    candidate
        .agents
        .retain(|item| item.spec.id != definition.spec.id);
    candidate.agents.push(definition.clone());
    candidate
        .validate()
        .map_err(|error| AgentRegistryError::Invalid(error.to_string()))
}

fn insert_definition(
    transaction: &Transaction<'_>,
    definition: &AgentDefinitionConfig,
    activate_if_new: bool,
) -> Result<(), AgentRegistryError> {
    let json = serde_json::to_string(definition).map_err(AgentRegistryError::serde)?;
    let digest = hex_digest(json.as_bytes());
    let revision = sql_revision(definition.revision)?;
    let existing: Option<String> = transaction
        .query_row(
            "SELECT digest FROM agent_definitions WHERE agent_id=?1 AND revision=?2",
            params![definition.spec.id.0, revision],
            |row| row.get(0),
        )
        .optional()
        .map_err(AgentRegistryError::sqlite)?;
    if let Some(existing) = existing {
        return if existing == digest {
            Ok(())
        } else {
            Err(AgentRegistryError::RevisionCollision {
                agent_id: definition.spec.id.0.clone(),
                revision: definition.revision,
            })
        };
    }
    transaction
        .execute(
            "INSERT INTO agent_definitions(agent_id, revision, definition_json, digest, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![definition.spec.id.0, revision, json, digest, now()],
        )
        .map_err(AgentRegistryError::sqlite)?;
    if activate_if_new {
        transaction
            .execute(
                "INSERT OR IGNORE INTO agent_registry_heads(agent_id, active_revision, updated_at) \
                 VALUES (?1, ?2, ?3)",
                params![definition.spec.id.0, revision, now()],
            )
            .map_err(AgentRegistryError::sqlite)?;
    }
    Ok(())
}

fn set_active(
    connection: &mut Connection,
    agent_id: &str,
    target: u64,
    expected: u64,
    rollback: bool,
) -> Result<(), AgentRegistryError> {
    let transaction = connection
        .transaction()
        .map_err(AgentRegistryError::sqlite)?;
    let actual = active_revision(&transaction, agent_id)?
        .ok_or_else(|| AgentRegistryError::UnknownAgent(agent_id.to_owned()))?;
    if actual != expected {
        return Err(AgentRegistryError::Conflict {
            agent_id: agent_id.to_owned(),
            expected,
            actual,
        });
    }
    if rollback && target >= actual {
        return Err(AgentRegistryError::InvalidRollback { target, actual });
    }
    if load_revision(&transaction, agent_id, target)?.is_none() {
        return Err(AgentRegistryError::UnknownRevision {
            agent_id: agent_id.to_owned(),
            revision: target,
        });
    }
    let target = sql_revision(target)?;
    transaction
        .execute(
            "UPDATE agent_registry_heads SET active_revision=?2, updated_at=?3 WHERE agent_id=?1",
            params![agent_id, target, now()],
        )
        .map_err(AgentRegistryError::sqlite)?;
    transaction.commit().map_err(AgentRegistryError::sqlite)
}

fn active_revision(
    connection: &Connection,
    agent_id: &str,
) -> Result<Option<u64>, AgentRegistryError> {
    connection
        .query_row(
            "SELECT active_revision FROM agent_registry_heads WHERE agent_id=?1",
            [agent_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(AgentRegistryError::sqlite)?
        .map(decode_revision)
        .transpose()
}

fn load_revision(
    connection: &Connection,
    agent_id: &str,
    revision: u64,
) -> Result<Option<AgentRevision>, AgentRegistryError> {
    let active = active_revision(connection, agent_id)?;
    let sql_revision = sql_revision(revision)?;
    connection
        .query_row(
            "SELECT definition_json, digest, created_at FROM agent_definitions \
             WHERE agent_id=?1 AND revision=?2",
            params![agent_id, sql_revision],
            |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(AgentRegistryError::sqlite)?
        .map(|(json, digest, created_at)| {
            decode_stored_revision(
                agent_id,
                revision,
                json,
                digest,
                created_at,
                active == Some(revision),
            )
        })
        .transpose()
}

fn decode_stored_revision(
    agent_id: &str,
    revision: u64,
    json: String,
    digest: String,
    created_at: i64,
    active: bool,
) -> Result<AgentRevision, AgentRegistryError> {
    let actual_digest = hex_digest(json.as_bytes());
    if actual_digest != digest {
        return Err(AgentRegistryError::Integrity(format!(
            "Agent revision `{agent_id}`@{revision} digest mismatch"
        )));
    }
    let definition = decode_definition(&json)?;
    if definition.spec.id.0 != agent_id || definition.revision != revision {
        return Err(AgentRegistryError::Integrity(format!(
            "Agent revision `{agent_id}`@{revision} identity does not match stored definition"
        )));
    }
    Ok(AgentRevision {
        definition,
        digest,
        created_at,
        active,
    })
}

fn decode_definition(json: &str) -> Result<AgentDefinitionConfig, AgentRegistryError> {
    serde_json::from_str(json).map_err(AgentRegistryError::serde)
}

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .cast_signed()
}

fn sql_revision(revision: u64) -> Result<i64, AgentRegistryError> {
    i64::try_from(revision)
        .map_err(|_| AgentRegistryError::Invalid("revision exceeds SQLite range".into()))
}

fn decode_revision(revision: i64) -> Result<u64, AgentRegistryError> {
    u64::try_from(revision)
        .map_err(|_| AgentRegistryError::Storage("stored revision is negative".into()))
}

#[derive(Debug, thiserror::Error)]
pub enum AgentRegistryError {
    #[error("invalid Agent definition: {0}")]
    Invalid(String),
    #[error("unknown Agent `{0}`")]
    UnknownAgent(String),
    #[error("unknown Agent revision `{agent_id}`@{revision}")]
    UnknownRevision { agent_id: String, revision: u64 },
    #[error("Agent `{agent_id}` active revision conflict: expected {expected}, found {actual}")]
    Conflict {
        agent_id: String,
        expected: u64,
        actual: u64,
    },
    #[error("Agent `{agent_id}` next revision must be {expected}, found {actual}")]
    NonSequential {
        agent_id: String,
        expected: u64,
        actual: u64,
    },
    #[error("Agent `{agent_id}` revision {revision} has different content")]
    RevisionCollision { agent_id: String, revision: u64 },
    #[error("rollback target {target} is not older than active revision {actual}")]
    InvalidRollback { target: u64, actual: u64 },
    #[error("Agent registry storage error: {0}")]
    Storage(String),
    #[error("Agent registry serialization error: {0}")]
    Serialization(String),
    #[error("Agent registry integrity error: {0}")]
    Integrity(String),
    #[error("Agent registry task failed: {0}")]
    Task(String),
}

impl AgentRegistryError {
    pub(crate) fn sqlite(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }

    pub(crate) fn serde(error: serde_json::Error) -> Self {
        Self::Serialization(error.to_string())
    }
}

fn initialize_or_validate_registry_schema(
    connection: &mut Connection,
    allowed_foreign_objects: &[String],
) -> Result<(), AgentRegistryError> {
    validate_allowed_foreign_objects(allowed_foreign_objects)?;
    let objects = registry_schema_objects(connection)?;
    validate_registry_object_namespace(&objects, allowed_foreign_objects)?;
    let has_registry_objects = objects
        .iter()
        .any(|object| REGISTRY_SCHEMA_OBJECT_NAMES.contains(&object.1.as_str()));
    if !has_registry_objects {
        let transaction = connection
            .transaction()
            .map_err(AgentRegistryError::sqlite)?;
        for schema in [
            SCHEMA,
            MIGRATION_SCHEMA,
            REGISTRY_CATALOG_SCHEMA,
            REGISTRY_SCHEMA_V3,
        ] {
            transaction
                .execute_batch(schema)
                .map_err(AgentRegistryError::sqlite)?;
        }
        transaction
            .execute(
                "INSERT INTO schema_migrations(component,version,applied_at) VALUES (?1,?2,?3)",
                params![REGISTRY_COMPONENT, REGISTRY_SCHEMA_VERSION, now()],
            )
            .map_err(AgentRegistryError::sqlite)?;
        transaction.commit().map_err(AgentRegistryError::sqlite)?;
    }
    ensure_current_registry_schema(connection)
}

pub(crate) fn ensure_current_registry_schema(
    connection: &Connection,
) -> Result<(), AgentRegistryError> {
    let expected = canonical_registry_schema()?;
    let actual = owned_registry_schema_objects(registry_schema_objects(connection)?);
    if actual != expected {
        return Err(AgentRegistryError::Integrity(
            "registry schema does not exactly match the current version".into(),
        ));
    }

    let mut statement = connection
        .prepare(
            "SELECT component,version,applied_at FROM schema_migrations \
             ORDER BY component,version",
        )
        .map_err(AgentRegistryError::sqlite)?;
    let ledger = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(AgentRegistryError::sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(AgentRegistryError::sqlite)?;
    if ledger.len() != 1
        || ledger[0].0 != REGISTRY_COMPONENT
        || ledger[0].1 != REGISTRY_SCHEMA_VERSION
        || ledger[0].2 <= 0
    {
        return Err(AgentRegistryError::Integrity(
            "registry schema ledger does not exactly match the current version".into(),
        ));
    }

    let owned = REGISTRY_SCHEMA_OBJECT_NAMES
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut foreign_keys = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(AgentRegistryError::sqlite)?;
    let mut rows = foreign_keys.query([]).map_err(AgentRegistryError::sqlite)?;
    while let Some(row) = rows.next().map_err(AgentRegistryError::sqlite)? {
        let table = row
            .get::<_, String>(0)
            .map_err(AgentRegistryError::sqlite)?;
        if owned.contains(table.as_str()) {
            return Err(AgentRegistryError::Integrity(
                "registry foreign-key integrity check failed".into(),
            ));
        }
    }
    Ok(())
}

fn validate_allowed_foreign_objects(
    allowed_foreign_objects: &[String],
) -> Result<(), AgentRegistryError> {
    let owned = REGISTRY_SCHEMA_OBJECT_NAMES
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut allowed = HashSet::new();
    for name in allowed_foreign_objects {
        if name.is_empty() || owned.contains(name.as_str()) || !allowed.insert(name.as_str()) {
            return Err(AgentRegistryError::Integrity(
                "registry shared-object allowlist is invalid".into(),
            ));
        }
    }
    Ok(())
}

fn validate_registry_object_namespace(
    objects: &[RegistrySchemaObject],
    allowed_foreign_objects: &[String],
) -> Result<(), AgentRegistryError> {
    let owned = REGISTRY_SCHEMA_OBJECT_NAMES
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let allowed = allowed_foreign_objects
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if objects
        .iter()
        .all(|object| owned.contains(object.1.as_str()) || allowed.contains(object.1.as_str()))
    {
        Ok(())
    } else {
        Err(AgentRegistryError::Integrity(
            "registry database contains an undeclared schema object".into(),
        ))
    }
}

fn owned_registry_schema_objects(objects: Vec<RegistrySchemaObject>) -> Vec<RegistrySchemaObject> {
    let owned = REGISTRY_SCHEMA_OBJECT_NAMES
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    objects
        .into_iter()
        .filter(|object| owned.contains(object.1.as_str()))
        .collect()
}

fn canonical_registry_schema() -> Result<Vec<RegistrySchemaObject>, AgentRegistryError> {
    let canonical = Connection::open_in_memory().map_err(AgentRegistryError::sqlite)?;
    canonical
        .execute_batch("PRAGMA foreign_keys=ON;")
        .map_err(AgentRegistryError::sqlite)?;
    for schema in [
        SCHEMA,
        MIGRATION_SCHEMA,
        REGISTRY_CATALOG_SCHEMA,
        REGISTRY_SCHEMA_V3,
    ] {
        canonical
            .execute_batch(schema)
            .map_err(AgentRegistryError::sqlite)?;
    }
    registry_schema_objects(&canonical)
}

type RegistrySchemaObject = (String, String, String, String);

fn registry_schema_objects(
    connection: &Connection,
) -> Result<Vec<RegistrySchemaObject>, AgentRegistryError> {
    let mut statement = connection
        .prepare(
            "SELECT type,name,tbl_name,COALESCE(sql,'') FROM sqlite_schema \
             WHERE name NOT LIKE 'sqlite_%' AND type IN ('table','index','trigger','view') \
             ORDER BY type,name",
        )
        .map_err(AgentRegistryError::sqlite)?;
    statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(AgentRegistryError::sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(AgentRegistryError::sqlite)
}

const REGISTRY_COMPONENT: &str = "runtime_registry";
const REGISTRY_SCHEMA_VERSION: i64 = 3;

/// `SQLite` objects owned and exact-match validated by the current registry.
pub const REGISTRY_SCHEMA_OBJECT_NAMES: &[&str] = &[
    "schema_migrations",
    "agent_definitions",
    "agent_registry_heads",
    "credential_binding_revisions",
    "credential_binding_heads",
    "provider_definitions",
    "provider_registry_heads",
    "model_definitions",
    "model_registry_heads",
    "agent_registry_snapshots_v3",
    "agent_registry_snapshot_providers_v3",
    "agent_registry_snapshot_models_v3",
    "one_default_model_per_agent_snapshot_v3",
];

const MIGRATION_SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS schema_migrations (
    component TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(version > 0),
    applied_at INTEGER NOT NULL,
    PRIMARY KEY(component, version)
);
";

const REGISTRY_CATALOG_SCHEMA: &str = r"
CREATE TABLE credential_binding_revisions (
    binding_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    reference_json TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(binding_id, generation)
);
CREATE TABLE credential_binding_heads (
    binding_id TEXT PRIMARY KEY,
    active_generation INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY(binding_id, active_generation)
        REFERENCES credential_binding_revisions(binding_id, generation)
);
CREATE TABLE provider_definitions (
    provider_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    definition_json TEXT NOT NULL,
    digest TEXT NOT NULL,
    credential_binding_id TEXT NOT NULL REFERENCES credential_binding_heads(binding_id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(provider_id, revision)
);
CREATE TABLE provider_registry_heads (
    provider_id TEXT PRIMARY KEY,
    active_revision INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY(provider_id, active_revision)
        REFERENCES provider_definitions(provider_id, revision)
);
CREATE TABLE model_definitions (
    provider_id TEXT NOT NULL REFERENCES provider_registry_heads(provider_id),
    model_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    definition_json TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(provider_id, model_id, revision)
);
CREATE TABLE model_registry_heads (
    provider_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    active_revision INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(provider_id, model_id),
    FOREIGN KEY(provider_id, model_id, active_revision)
        REFERENCES model_definitions(provider_id, model_id, revision)
);
";

// The current schema creates every required snapshot table atomically. Older
// snapshot table shapes are not part of a valid database and fail the exact
// schema check during open.
const REGISTRY_SCHEMA_V3: &str = r"
CREATE TABLE agent_registry_snapshots_v3 (
    agent_id TEXT NOT NULL CHECK(length(trim(agent_id)) > 0),
    agent_revision INTEGER NOT NULL CHECK(agent_revision > 0),
    default_provider_id TEXT NOT NULL CHECK(length(trim(default_provider_id)) > 0),
    default_model_id TEXT NOT NULL CHECK(length(trim(default_model_id)) > 0),
    default_marker INTEGER NOT NULL DEFAULT 1 CHECK(default_marker = 1),
    digest TEXT NOT NULL CHECK(length(trim(digest)) > 0),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(agent_id, agent_revision),
    FOREIGN KEY(agent_id, agent_revision)
        REFERENCES agent_definitions(agent_id, revision),
    FOREIGN KEY(
        agent_id,
        agent_revision,
        default_provider_id,
        default_model_id,
        default_marker
    ) REFERENCES agent_registry_snapshot_models_v3(
        agent_id,
        agent_revision,
        provider_id,
        model_id,
        is_default
    ) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE agent_registry_snapshot_providers_v3 (
    agent_id TEXT NOT NULL CHECK(length(trim(agent_id)) > 0),
    agent_revision INTEGER NOT NULL CHECK(agent_revision > 0),
    provider_id TEXT NOT NULL CHECK(length(trim(provider_id)) > 0),
    provider_revision INTEGER NOT NULL CHECK(provider_revision > 0),
    PRIMARY KEY(agent_id, agent_revision, provider_id),
    FOREIGN KEY(agent_id, agent_revision)
        REFERENCES agent_registry_snapshots_v3(agent_id, agent_revision),
    FOREIGN KEY(provider_id, provider_revision)
        REFERENCES provider_definitions(provider_id, revision)
);
CREATE TABLE agent_registry_snapshot_models_v3 (
    agent_id TEXT NOT NULL CHECK(length(trim(agent_id)) > 0),
    agent_revision INTEGER NOT NULL CHECK(agent_revision > 0),
    provider_id TEXT NOT NULL CHECK(length(trim(provider_id)) > 0),
    model_id TEXT NOT NULL CHECK(length(trim(model_id)) > 0),
    model_revision INTEGER NOT NULL CHECK(model_revision > 0),
    is_default INTEGER NOT NULL CHECK(is_default IN (0, 1)),
    PRIMARY KEY(agent_id, agent_revision, provider_id, model_id),
    UNIQUE(agent_id, agent_revision, provider_id, model_id, is_default),
    FOREIGN KEY(agent_id, agent_revision, provider_id)
        REFERENCES agent_registry_snapshot_providers_v3(
            agent_id,
            agent_revision,
            provider_id
        ),
    FOREIGN KEY(provider_id, model_id, model_revision)
        REFERENCES model_definitions(provider_id, model_id, revision)
);
CREATE UNIQUE INDEX one_default_model_per_agent_snapshot_v3
    ON agent_registry_snapshot_models_v3(agent_id, agent_revision)
    WHERE is_default = 1;
";

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS agent_definitions (
    agent_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    definition_json TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(agent_id, revision)
);
CREATE TABLE IF NOT EXISTS agent_registry_heads (
    agent_id TEXT PRIMARY KEY,
    active_revision INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY(agent_id, active_revision)
        REFERENCES agent_definitions(agent_id, revision)
);
";

#[cfg(test)]
#[path = "../tests/unit/agent_registry.rs"]
mod tests;
