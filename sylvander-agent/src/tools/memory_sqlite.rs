//! SQLite-backed relationship memory with versioned, fail-closed migrations.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use sylvander_protocol::SessionContext;
use sylvander_protocol::types::{AgentId, UserId};

use super::memory::{
    Importance, MAX_MEMORY_QUERY_BYTES, MAX_MEMORY_RESULTS, MemoryEntry, MemoryFilter, MemoryOwner,
    MemoryStore, MemoryStoreError, same_owner, validate_entry,
};

const COMPONENT: &str = "relationship_memory";
const SCHEMA_VERSION: i64 = 1;
const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS relationship_memories (
  owner_user TEXT NOT NULL,
  owner_agent TEXT NOT NULL,
  id TEXT NOT NULL,
  kind_json TEXT NOT NULL,
  content TEXT NOT NULL,
  references_json TEXT NOT NULL,
  tags_json TEXT NOT NULL,
  importance INTEGER NOT NULL CHECK (importance BETWEEN 0 AND 3),
  created_at INTEGER NOT NULL,
  last_accessed INTEGER,
  access_count INTEGER NOT NULL CHECK (access_count >= 0),
  metadata_json TEXT NOT NULL,
  PRIMARY KEY (owner_user, owner_agent, id)
);
CREATE INDEX IF NOT EXISTS relationship_memories_search
  ON relationship_memories(owner_user, owner_agent, importance DESC, created_at DESC, id ASC);
";

/// Durable implementation of the relationship-only [`MemoryStore`] contract.
#[derive(Clone)]
pub struct SqliteMemoryStore {
    connection: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for SqliteMemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteMemoryStore").finish_non_exhaustive()
    }
}

impl SqliteMemoryStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryStoreError> {
        Self::from_connection(Connection::open(path).map_err(store_error)?)
    }

    pub fn open_in_memory() -> Result<Self, MemoryStoreError> {
        Self::from_connection(Connection::open_in_memory().map_err(store_error)?)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, MemoryStoreError> {
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(store_error)?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    fn with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, MemoryStoreError>,
    ) -> Result<T, MemoryStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| MemoryStoreError::Store("memory database lock poisoned".into()))?;
        operation(&connection)
    }
}

#[async_trait]
impl MemoryStore for SqliteMemoryStore {
    async fn search(
        &self,
        ctx: &SessionContext,
        query: &str,
        filter: MemoryFilter,
    ) -> Result<Vec<MemoryEntry>, MemoryStoreError> {
        if query.len() > MAX_MEMORY_QUERY_BYTES
            || filter
                .limit
                .is_some_and(|limit| limit == 0 || limit > MAX_MEMORY_RESULTS)
        {
            return Err(MemoryStoreError::InvalidInput);
        }
        let owner = MemoryOwner::relationship(ctx);
        let MemoryOwner::Relationship { user_id, agent_id } = owner else {
            unreachable!("relationship constructor returned another scope")
        };
        let query = query.to_lowercase();
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT id, kind_json, content, references_json, tags_json, importance, created_at, last_accessed, access_count, metadata_json FROM relationship_memories WHERE owner_user = ?1 AND owner_agent = ?2 ORDER BY importance DESC, created_at DESC, id ASC")
                .map_err(search_error)?;
            let rows = statement
                .query_map(params![user_id.0, agent_id.0], |row| decode_row(row, &user_id, &agent_id))
                .map_err(search_error)?;
            let mut results = Vec::new();
            for row in rows {
                let entry = row.map_err(search_error)?;
                if (!query.is_empty() && !entry.content.to_lowercase().contains(&query))
                    || filter.kind.as_ref().is_some_and(|kind| kind != &entry.kind)
                    || filter
                        .min_importance
                        .is_some_and(|importance| entry.importance < importance)
                {
                    continue;
                }
                results.push(entry);
                if filter.limit == Some(results.len()) {
                    break;
                }
            }
            Ok(results)
        })
    }

    async fn store(
        &self,
        ctx: &SessionContext,
        entry: MemoryEntry,
    ) -> Result<(), MemoryStoreError> {
        validate_entry(&entry)?;
        if !same_owner(&entry.owner, ctx) {
            return Err(MemoryStoreError::AccessDenied(
                "memory owner does not match the runtime context".into(),
            ));
        }
        let MemoryOwner::Relationship { user_id, agent_id } = &entry.owner else {
            return Err(MemoryStoreError::AccessDenied(
                "only relationship memory is accepted by this store".into(),
            ));
        };
        let kind = encode(&entry.kind)?;
        let references = encode(&entry.references)?;
        let tags = encode(&entry.tags)?;
        let metadata = encode(&entry.metadata)?;
        self.with_connection(|connection| {
            connection.execute(
                "INSERT INTO relationship_memories (owner_user, owner_agent, id, kind_json, content, references_json, tags_json, importance, created_at, last_accessed, access_count, metadata_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![user_id.0, agent_id.0, entry.id, kind, entry.content, references, tags, importance_value(entry.importance), entry.created_at, entry.last_accessed, entry.access_count, metadata],
            ).map_err(|error| {
                if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) {
                    MemoryStoreError::Store("memory identity already exists".into())
                } else {
                    store_error(error)
                }
            })?;
            Ok(())
        })
    }

    async fn delete(&self, ctx: &SessionContext, id: &str) -> Result<(), MemoryStoreError> {
        let MemoryOwner::Relationship { user_id, agent_id } = MemoryOwner::relationship(ctx) else {
            unreachable!("relationship constructor returned another scope")
        };
        self.with_connection(|connection| {
            let changed = connection
                .execute(
                    "DELETE FROM relationship_memories WHERE owner_user = ?1 AND owner_agent = ?2 AND id = ?3",
                    params![user_id.0, agent_id.0, id],
                )
                .map_err(delete_error)?;
            if changed == 0 {
                return Err(MemoryStoreError::AccessDenied("memory is not visible".into()));
            }
            Ok(())
        })
    }

    async fn get(
        &self,
        ctx: &SessionContext,
        id: &str,
    ) -> Result<Option<MemoryEntry>, MemoryStoreError> {
        let MemoryOwner::Relationship { user_id, agent_id } = MemoryOwner::relationship(ctx) else {
            unreachable!("relationship constructor returned another scope")
        };
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT id, kind_json, content, references_json, tags_json, importance, created_at, last_accessed, access_count, metadata_json FROM relationship_memories WHERE owner_user = ?1 AND owner_agent = ?2 AND id = ?3",
                    params![user_id.0, agent_id.0, id],
                    |row| decode_row(row, &user_id, &agent_id),
                )
                .optional()
                .map_err(store_error)
        })
    }
}

fn migrate(connection: &mut Connection) -> Result<(), MemoryStoreError> {
    connection
        .execute_batch("PRAGMA foreign_keys = ON; CREATE TABLE IF NOT EXISTS memory_schema_migrations (component TEXT PRIMARY KEY, version INTEGER NOT NULL CHECK (version > 0));")
        .map_err(store_error)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(store_error)?;
    let version: Option<i64> = transaction
        .query_row(
            "SELECT version FROM memory_schema_migrations WHERE component = ?1",
            [COMPONENT],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_error)?;
    match version {
        None => {
            transaction.execute_batch(SCHEMA).map_err(store_error)?;
            transaction
                .execute(
                    "INSERT INTO memory_schema_migrations(component, version) VALUES (?1, ?2)",
                    params![COMPONENT, SCHEMA_VERSION],
                )
                .map_err(store_error)?;
            transaction.commit().map_err(store_error)
        }
        Some(SCHEMA_VERSION) => {
            transaction
                .prepare("SELECT owner_user, owner_agent, id, kind_json, content, references_json, tags_json, importance, created_at, last_accessed, access_count, metadata_json FROM relationship_memories LIMIT 0")
                .map_err(store_error)?;
            transaction.commit().map_err(store_error)
        }
        Some(_) => Err(MemoryStoreError::Store(
            "unsupported relationship memory schema version".into(),
        )),
    }
}

fn decode_row(
    row: &rusqlite::Row<'_>,
    user_id: &UserId,
    agent_id: &AgentId,
) -> rusqlite::Result<MemoryEntry> {
    let importance: i64 = row.get(5)?;
    Ok(MemoryEntry {
        id: row.get(0)?,
        owner: MemoryOwner::Relationship {
            user_id: user_id.clone(),
            agent_id: agent_id.clone(),
        },
        kind: decode(row, 1)?,
        content: row.get(2)?,
        references: decode(row, 3)?,
        tags: decode(row, 4)?,
        importance: decode_importance(importance, 5)?,
        created_at: row.get(6)?,
        last_accessed: row.get(7)?,
        access_count: row.get(8)?,
        metadata: decode(row, 9)?,
    })
}

fn encode(value: &impl serde::Serialize) -> Result<String, MemoryStoreError> {
    serde_json::to_string(value).map_err(|error| MemoryStoreError::Store(error.to_string()))
}

fn decode<T: DeserializeOwned>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T> {
    let value: String = row.get(index)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

const fn importance_value(importance: Importance) -> i64 {
    match importance {
        Importance::Low => 0,
        Importance::Medium => 1,
        Importance::High => 2,
        Importance::Critical => 3,
    }
}

fn decode_importance(value: i64, index: usize) -> rusqlite::Result<Importance> {
    match value {
        0 => Ok(Importance::Low),
        1 => Ok(Importance::Medium),
        2 => Ok(Importance::High),
        3 => Ok(Importance::Critical),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(index, value)),
    }
}

fn store_error(error: rusqlite::Error) -> MemoryStoreError {
    MemoryStoreError::Store(error.to_string())
}
fn search_error(error: rusqlite::Error) -> MemoryStoreError {
    MemoryStoreError::Search(error.to_string())
}
fn delete_error(error: rusqlite::Error) -> MemoryStoreError {
    MemoryStoreError::Delete(error.to_string())
}

#[cfg(test)]
#[path = "memory_sqlite_tests.rs"]
mod tests;
