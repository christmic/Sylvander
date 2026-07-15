//! SQLite-backed relationship memory with versioned, fail-closed migrations.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use sylvander_protocol::types::{AgentId, UserId};

use super::memory::{
    Importance, MAX_MEMORY_QUERY_BYTES, MAX_MEMORY_RESULTS, MemoryAppend, MemoryEntry,
    MemoryExecutionContext, MemoryFilter, MemoryOwner, MemoryProvenance, MemoryProvenanceSource,
    MemoryStore, MemoryStoreError, memory_not_visible, validate_append, validate_memory_id,
};

const COMPONENT: &str = "relationship_memory";
const SCHEMA_VERSION: i64 = 2;
const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS relationship_memories (
  record_key TEXT NOT NULL UNIQUE,
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
  revision INTEGER NOT NULL CHECK (revision >= 1),
  updated_at INTEGER NOT NULL,
  expires_at INTEGER,
  superseded_by_record_key TEXT,
  origin_actor_kind TEXT NOT NULL,
  origin_user_id TEXT,
  origin_agent_id TEXT,
  origin_session_id TEXT,
  origin_trace_id TEXT,
  origin_source TEXT NOT NULL,
  provenance_trusted INTEGER NOT NULL CHECK (provenance_trusted IN (0, 1)),
  PRIMARY KEY (owner_user, owner_agent, id)
);
CREATE INDEX IF NOT EXISTS relationship_memories_search
  ON relationship_memories(owner_user, owner_agent, importance DESC, created_at DESC, id ASC);
CREATE TABLE IF NOT EXISTS relationship_memory_audit (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  occurred_at INTEGER NOT NULL,
  operation TEXT NOT NULL,
  target_record_key TEXT NOT NULL,
  before_revision INTEGER,
  after_revision INTEGER,
  actor_kind TEXT NOT NULL,
  actor_user_id TEXT,
  actor_agent_id TEXT,
  session_id TEXT,
  trace_id TEXT,
  changed_mask INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS relationship_memory_audit_no_update
BEFORE UPDATE ON relationship_memory_audit BEGIN
  SELECT RAISE(ABORT, 'memory audit is append-only');
END;
CREATE TRIGGER IF NOT EXISTS relationship_memory_audit_no_delete
BEFORE DELETE ON relationship_memory_audit BEGIN
  SELECT RAISE(ABORT, 'memory audit is append-only');
END;
";
const ENTRY_SELECT: &str = "SELECT m.id, m.kind_json, m.content, m.references_json, m.tags_json, m.importance, m.created_at, m.last_accessed, m.access_count, m.metadata_json, m.revision, m.updated_at, m.expires_at, replacement.id, m.origin_actor_kind, m.origin_user_id, m.origin_agent_id, m.origin_session_id, m.origin_trace_id, m.origin_source, m.provenance_trusted FROM relationship_memories m LEFT JOIN relationship_memories replacement ON replacement.record_key = m.superseded_by_record_key";

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
        operation: impl FnOnce(&mut Connection) -> Result<T, MemoryStoreError>,
    ) -> Result<T, MemoryStoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| MemoryStoreError::Store("memory database lock poisoned".into()))?;
        operation(&mut connection)
    }
}

#[async_trait]
impl MemoryStore for SqliteMemoryStore {
    async fn append_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        append: MemoryAppend,
    ) -> Result<MemoryEntry, MemoryStoreError> {
        let owner = ctx.relationship_owner()?;
        validate_append(&append)?;
        let now = crate::session::now_secs();
        let entry = MemoryEntry::materialize(
            uuid::Uuid::new_v4().to_string(),
            owner,
            append,
            ctx.provenance(),
            now,
        )?;
        let MemoryOwner::Relationship { user_id, agent_id } = &entry.owner else {
            unreachable!("relationship context returned another scope")
        };
        let kind = encode(&entry.kind)?;
        let references = encode(&entry.references)?;
        let tags = encode(&entry.tags)?;
        let metadata = encode(&entry.metadata)?;
        let record_key = uuid::Uuid::new_v4().to_string();
        self.with_connection(|connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(store_error)?;
            transaction.execute(
                "INSERT INTO relationship_memories (record_key, owner_user, owner_agent, id, kind_json, content, references_json, tags_json, importance, created_at, last_accessed, access_count, metadata_json, revision, updated_at, expires_at, superseded_by_record_key, origin_actor_kind, origin_user_id, origin_agent_id, origin_session_id, origin_trace_id, origin_source, provenance_trusted) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, NULL, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
                params![record_key, user_id.0, agent_id.0, entry.id, kind, entry.content, references, tags, importance_value(entry.importance), entry.created_at, entry.last_accessed, entry.access_count, metadata, i64::try_from(entry.revision).map_err(|_| MemoryStoreError::InvalidInput)?, entry.updated_at, entry.expires_at, actor_value(entry.provenance.actor), option_id(entry.provenance.user_id.as_ref()), option_id(entry.provenance.agent_id.as_ref()), option_id(entry.provenance.session_id.as_ref()), entry.provenance.trace_id, source_value(entry.provenance.source), entry.provenance.trusted],
            ).map_err(store_error)?;
            append_audit(&transaction, ctx, &record_key, "append", None, Some(1), now, 0x3f)?;
            transaction.commit().map_err(store_error)
        })?;
        Ok(entry)
    }

    async fn search_relationship(
        &self,
        ctx: &MemoryExecutionContext,
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
        let owner = ctx.relationship_owner()?;
        let MemoryOwner::Relationship { user_id, agent_id } = owner else {
            unreachable!("relationship constructor returned another scope")
        };
        let query = query.to_lowercase();
        let now = crate::session::now_secs();
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare(&format!("{ENTRY_SELECT} WHERE m.owner_user = ?1 AND m.owner_agent = ?2 AND m.superseded_by_record_key IS NULL AND (m.expires_at IS NULL OR m.expires_at > ?3) ORDER BY m.importance DESC, m.created_at DESC, m.id ASC"))
                .map_err(search_error)?;
            let rows = statement
                .query_map(params![user_id.0, agent_id.0, now], |row| decode_row(row, &user_id, &agent_id))
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

    async fn delete_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
        expected_revision: u64,
    ) -> Result<(), MemoryStoreError> {
        let MemoryOwner::Relationship { user_id, agent_id } = ctx.relationship_owner()? else {
            unreachable!("relationship constructor returned another scope")
        };
        validate_memory_id(id)?;
        if expected_revision == 0 {
            return Err(MemoryStoreError::InvalidInput);
        }
        let expected_revision =
            i64::try_from(expected_revision).map_err(|_| MemoryStoreError::InvalidInput)?;
        let now = crate::session::now_secs();
        self.with_connection(|connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(delete_error)?;
            let visible: Option<(String, i64)> = transaction
                .query_row(
                    "SELECT record_key, revision FROM relationship_memories WHERE owner_user = ?1 AND owner_agent = ?2 AND id = ?3",
                    params![user_id.0, agent_id.0, id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(delete_error)?;
            let Some((record_key, revision)) = visible else {
                return Err(memory_not_visible());
            };
            if revision != expected_revision {
                return Err(MemoryStoreError::Conflict);
            }
            let changed = transaction
                .execute(
                    "DELETE FROM relationship_memories WHERE record_key = ?1 AND revision = ?2",
                    params![record_key, expected_revision],
                )
                .map_err(delete_error)?;
            if changed != 1 {
                return Err(MemoryStoreError::Conflict);
            }
            append_audit(
                &transaction,
                ctx,
                &record_key,
                "delete",
                u64::try_from(revision).ok(),
                None,
                now,
                0,
            )?;
            transaction.commit().map_err(delete_error)
        })
    }

    async fn get_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
    ) -> Result<Option<MemoryEntry>, MemoryStoreError> {
        let MemoryOwner::Relationship { user_id, agent_id } = ctx.relationship_owner()? else {
            unreachable!("relationship constructor returned another scope")
        };
        validate_memory_id(id)?;
        let now = crate::session::now_secs();
        self.with_connection(|connection| {
            connection
                .query_row(
                    &format!("{ENTRY_SELECT} WHERE m.owner_user = ?1 AND m.owner_agent = ?2 AND m.id = ?3 AND m.superseded_by_record_key IS NULL AND (m.expires_at IS NULL OR m.expires_at > ?4)"),
                    params![user_id.0, agent_id.0, id, now],
                    |row| decode_row(row, &user_id, &agent_id),
                )
                .optional()
                .map_err(store_error)
        })
    }
}

fn migrate(connection: &mut Connection) -> Result<(), MemoryStoreError> {
    let has_component_objects = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN ('relationship_memories', 'relationship_memories_search', 'relationship_memory_audit', 'relationship_memory_audit_no_update', 'relationship_memory_audit_no_delete'))",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|_| schema_error())?;
    let has_ledger = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memory_schema_migrations')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|_| schema_error())?;
    if has_component_objects && !has_ledger {
        return Err(schema_error());
    }
    connection
        .execute_batch("PRAGMA foreign_keys = ON; CREATE TABLE IF NOT EXISTS memory_schema_migrations (component TEXT PRIMARY KEY, version INTEGER NOT NULL CHECK (version > 0));")
        .map_err(|_| schema_error())?;
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
        .map_err(|_| schema_error())?;
    match version {
        None => {
            if component_objects_exist(&transaction)? {
                return Err(schema_error());
            }
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
                .prepare(&format!("{ENTRY_SELECT} LIMIT 0"))
                .map_err(|_| schema_error())?;
            transaction
                .prepare("SELECT sequence, event_id, occurred_at, operation, target_record_key, before_revision, after_revision, actor_kind, actor_user_id, actor_agent_id, session_id, trace_id, changed_mask FROM relationship_memory_audit LIMIT 0")
                .map_err(|_| schema_error())?;
            let trigger_count: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name IN ('relationship_memory_audit_no_update', 'relationship_memory_audit_no_delete')",
                    [],
                    |row| row.get(0),
                )
                .map_err(|_| schema_error())?;
            if trigger_count != 2 {
                return Err(schema_error());
            }
            transaction.commit().map_err(store_error)
        }
        Some(_) => Err(schema_error()),
    }
}

fn component_objects_exist(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<bool, MemoryStoreError> {
    transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN ('relationship_memories', 'relationship_memories_search', 'relationship_memory_audit', 'relationship_memory_audit_no_update', 'relationship_memory_audit_no_delete'))",
            [],
            |row| row.get(0),
        )
        .map_err(|_| schema_error())
}

fn schema_error() -> MemoryStoreError {
    MemoryStoreError::Store("unsupported relationship memory schema".into())
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
        revision: read_revision(row, 10)?,
        updated_at: row.get(11)?,
        expires_at: row.get(12)?,
        superseded_by: row.get(13)?,
        provenance: MemoryProvenance {
            actor: parse_actor(row.get::<_, String>(14)?.as_str(), 14)?,
            user_id: row.get::<_, Option<String>>(15)?.map(UserId::new),
            agent_id: row.get::<_, Option<String>>(16)?.map(AgentId::new),
            session_id: row
                .get::<_, Option<String>>(17)?
                .map(sylvander_protocol::types::SessionId::new),
            trace_id: row.get(18)?,
            source: parse_source(row.get::<_, String>(19)?.as_str(), 19)?,
            trusted: row.get::<_, i64>(20)? == 1,
        },
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

fn read_revision(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value)
        .ok()
        .filter(|revision| *revision > 0)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(index, value))
}

const fn actor_value(actor: super::memory::MemoryActorKind) -> &'static str {
    match actor {
        super::memory::MemoryActorKind::Worker => "worker",
        super::memory::MemoryActorKind::Guardian => "guardian",
        super::memory::MemoryActorKind::SystemService => "system_service",
    }
}

fn parse_actor(value: &str, index: usize) -> rusqlite::Result<super::memory::MemoryActorKind> {
    match value {
        "worker" => Ok(super::memory::MemoryActorKind::Worker),
        "guardian" => Ok(super::memory::MemoryActorKind::Guardian),
        "system_service" => Ok(super::memory::MemoryActorKind::SystemService),
        _ => Err(invalid_text(index, "invalid memory actor")),
    }
}

const fn source_value(source: MemoryProvenanceSource) -> &'static str {
    match source {
        MemoryProvenanceSource::Runtime => "runtime",
    }
}

fn parse_source(value: &str, index: usize) -> rusqlite::Result<MemoryProvenanceSource> {
    match value {
        "runtime" => Ok(MemoryProvenanceSource::Runtime),
        _ => Err(invalid_text(index, "invalid memory provenance source")),
    }
}

fn invalid_text(index: usize, message: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        std::io::Error::new(std::io::ErrorKind::InvalidData, message).into(),
    )
}

fn option_id<T: std::fmt::Display>(value: Option<&T>) -> Option<String> {
    value.map(ToString::to_string)
}

#[allow(clippy::too_many_arguments)]
fn append_audit(
    transaction: &rusqlite::Transaction<'_>,
    ctx: &MemoryExecutionContext,
    record_key: &str,
    operation: &str,
    before_revision: Option<u64>,
    after_revision: Option<u64>,
    occurred_at: i64,
    changed_mask: i64,
) -> Result<(), MemoryStoreError> {
    let before = before_revision
        .map(i64::try_from)
        .transpose()
        .map_err(|_| MemoryStoreError::InvalidInput)?;
    let after = after_revision
        .map(i64::try_from)
        .transpose()
        .map_err(|_| MemoryStoreError::InvalidInput)?;
    transaction
        .execute(
            "INSERT INTO relationship_memory_audit (event_id, occurred_at, operation, target_record_key, before_revision, after_revision, actor_kind, actor_user_id, actor_agent_id, session_id, trace_id, changed_mask) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![uuid::Uuid::new_v4().to_string(), occurred_at, operation, record_key, before, after, actor_value(ctx.actor()), option_id(ctx.user_id()), option_id(ctx.agent_id()), option_id(ctx.session_id()), ctx.trace_id(), changed_mask],
        )
        .map_err(store_error)?;
    Ok(())
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

#[cfg(test)]
#[path = "memory_sqlite_v2_tests.rs"]
mod v2_tests;
