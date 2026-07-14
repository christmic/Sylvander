//! Durable, immutable Agent definitions and explicit revision activation.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use sylvander_protocol::AgentId;
use tokio::sync::Mutex;
use tokio::task;

use crate::config::{AgentDefinitionConfig, ServerConfig};

#[derive(Clone)]
pub struct AgentRegistry {
    connection: Arc<Mutex<Connection>>,
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
        Self::open_connection(move || Connection::open(path)).await
    }

    #[cfg(test)]
    async fn open_in_memory() -> Result<Self, AgentRegistryError> {
        Self::open_connection(Connection::open_in_memory).await
    }

    async fn open_connection(
        open: impl FnOnce() -> rusqlite::Result<Connection> + Send + 'static,
    ) -> Result<Self, AgentRegistryError> {
        task::spawn_blocking(move || {
            let mut connection = open().map_err(AgentRegistryError::sqlite)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(AgentRegistryError::sqlite)?;
            connection
                .execute_batch("PRAGMA foreign_keys=ON;")
                .map_err(AgentRegistryError::sqlite)?;
            connection
                .execute_batch(SCHEMA)
                .map_err(AgentRegistryError::sqlite)?;
            run_registry_migrations(&mut connection)?;
            Ok(Self {
                connection: Arc::new(Mutex::new(connection)),
            })
        })
        .await
        .map_err(|error| AgentRegistryError::Task(error.to_string()))?
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, AgentRegistryError> + Send + 'static,
    ) -> Result<T, AgentRegistryError> {
        let connection = self.connection.clone();
        task::spawn_blocking(move || {
            let mut connection = connection.blocking_lock();
            operation(&mut connection)
        })
        .await
        .map_err(|error| AgentRegistryError::Task(error.to_string()))?
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
    fn sqlite(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }

    fn serde(error: serde_json::Error) -> Self {
        Self::Serialization(error.to_string())
    }
}

fn run_registry_migrations(connection: &mut Connection) -> Result<(), AgentRegistryError> {
    connection
        .execute_batch(MIGRATION_SCHEMA)
        .map_err(AgentRegistryError::sqlite)?;
    let current = connection
        .query_row(
            "SELECT MAX(version) FROM schema_migrations WHERE component=?1",
            [REGISTRY_COMPONENT],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(AgentRegistryError::sqlite)?
        .unwrap_or(0);
    if current > REGISTRY_SCHEMA_VERSION {
        return Err(AgentRegistryError::Integrity(format!(
            "registry schema version {current} is newer than supported version {REGISTRY_SCHEMA_VERSION}"
        )));
    }
    for (version, migration) in REGISTRY_MIGRATIONS {
        if *version <= current {
            continue;
        }
        let transaction = connection
            .transaction()
            .map_err(AgentRegistryError::sqlite)?;
        transaction
            .execute_batch(migration)
            .map_err(AgentRegistryError::sqlite)?;
        transaction
            .execute(
                "INSERT INTO schema_migrations(component,version,applied_at) VALUES (?1,?2,?3)",
                params![REGISTRY_COMPONENT, version, now()],
            )
            .map_err(AgentRegistryError::sqlite)?;
        transaction.commit().map_err(AgentRegistryError::sqlite)?;
    }
    Ok(())
}

const REGISTRY_COMPONENT: &str = "runtime_registry";
const REGISTRY_SCHEMA_VERSION: i64 = 1;
const REGISTRY_MIGRATIONS: &[(i64, &str)] = &[(1, REGISTRY_SCHEMA_V1)];

const MIGRATION_SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS schema_migrations (
    component TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(version > 0),
    applied_at INTEGER NOT NULL,
    PRIMARY KEY(component, version)
);
";

const REGISTRY_SCHEMA_V1: &str = r"
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
mod tests;
