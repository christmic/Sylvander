//! SQLite-backed [`SessionStore`].
//!
//! MVP concurrency model: a single `rusqlite::Connection` guarded by
//! `tokio::sync::Mutex`. All calls go through `spawn_blocking` so
//! `SQLite` work never stalls the async runtime. Adequate for desktop
//! use (single agent process, low write rate). A real production
//! deployment should swap this for `deadpool-sqlite` or `sqlx` with
//! a proper pool.
//!
//! Schema is created on first open (idempotent migration).
//!
//! Wire-format compatibility: `content_json` stores the same JSON
//! shape Anthropic uses for `MessageParam` / `Message`, so the
//! history can be fed straight back into `AgentLoop::run` after a
//! restart without re-serialization.

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use tokio::sync::Mutex;
use tokio::task;

use crate::session::SessionMetadata;
use crate::spec::{AgentId, SessionId};

use super::{
    MessageRole, ReplacementMessage, SessionFilter, SessionLifetime, SessionMetadataPatch,
    SessionStore, SessionStoreError, SessionUsage, StoredMessage, StoredSession,
    TurnConfigSnapshot, TurnStart,
};

/// SQLite-backed session store.
#[derive(Clone)]
pub struct SqliteSessionStore {
    inner: Arc<StoreInner>,
}

struct StoreInner {
    /// Synchronous `SQLite` connection. Guarded by `Mutex` so async tasks
    /// serialize their `spawn_blocking` calls into a single thread.
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for SqliteSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSessionStore").finish_non_exhaustive()
    }
}

impl SqliteSessionStore {
    /// Open or create a database at `path`. Runs migrations on first
    /// call; idempotent thereafter.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, SessionStoreError> {
        let path = path.as_ref().to_path_buf();
        task::spawn_blocking(move || -> Result<Self, SessionStoreError> {
            let conn = Connection::open(&path).map_err(sqlite_err)?;
            Self::init_schema(&conn)?;
            Ok(Self {
                inner: Arc::new(StoreInner {
                    conn: Mutex::new(conn),
                }),
            })
        })
        .await
        .map_err(|e| SessionStoreError::Store(format!("blocking task panicked: {e}")))?
    }

    /// In-memory `SQLite` (`:memory:`). Used in tests; supports the
    /// full schema so behavior matches a file-backed store.
    pub async fn open_in_memory() -> Result<Self, SessionStoreError> {
        task::spawn_blocking(|| -> Result<Self, SessionStoreError> {
            let conn = Connection::open_in_memory().map_err(sqlite_err)?;
            Self::init_schema(&conn)?;
            Ok(Self {
                inner: Arc::new(StoreInner {
                    conn: Mutex::new(conn),
                }),
            })
        })
        .await
        .map_err(|e| SessionStoreError::Store(format!("blocking task panicked: {e}")))?
    }

    /// One-shot schema bootstrap. Idempotent — uses `IF NOT EXISTS`.
    fn init_schema(conn: &Connection) -> Result<(), SessionStoreError> {
        conn.execute_batch(SCHEMA_SQL).map_err(sqlite_err)?;
        ensure_usage_cost_columns(conn)?;
        ensure_session_config_columns(conn)?;
        Ok(())
    }

    /// Acquire the lock and run a closure against the connection on
    /// a blocking thread. Centralizes the `spawn_blocking` boilerplate.
    ///
    /// The closure returns `Result<T, SessionStoreError>` directly
    /// (not `rusqlite::Result`) so it can return our own error type
    /// for things like `NotFound` without a lossy conversion.
    async fn run<F, T>(&self, f: F) -> Result<T, SessionStoreError>
    where
        F: FnOnce(&Connection) -> Result<T, SessionStoreError> + Send + 'static,
        T: Send + 'static,
    {
        let inner = self.inner.clone();
        task::spawn_blocking(move || {
            // We can't .await inside spawn_blocking, so we use
            // blocking_lock. SQLite is held briefly per call.
            let conn = inner.conn.blocking_lock();
            f(&conn)
        })
        .await
        .map_err(|e| SessionStoreError::Store(format!("blocking task panicked: {e}")))?
    }
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const SCHEMA_SQL: &str = r"
-- Session metadata
CREATE TABLE IF NOT EXISTS sessions (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    lifetime        TEXT NOT NULL,
    workspace       TEXT NOT NULL,
    user_id         TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    external_meta   TEXT NOT NULL DEFAULT '{}',
    config_revision INTEGER NOT NULL DEFAULT 0,
    config_overrides TEXT NOT NULL DEFAULT '{}',
    effective_config TEXT,
    is_archived     INTEGER NOT NULL DEFAULT 0,
    archive_reason  TEXT
);

-- Many-to-many: session ↔ agent
CREATE TABLE IF NOT EXISTS session_agents (
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_id        TEXT NOT NULL,
    joined_at       INTEGER NOT NULL,
    PRIMARY KEY (session_id, agent_id)
);

-- Messages (one row per user/assistant/tool message)
--
-- Identity / trace / priority are denormalized into real columns
-- (not a JSON blob) so SQLite can use indexes for per-user / per-
-- trace lookups. Adding a new SessionContext field means
-- `ALTER TABLE ADD COLUMN`, not editing a json blob.
CREATE TABLE IF NOT EXISTS session_messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    role            TEXT NOT NULL,
    content_json    TEXT NOT NULL,
    -- Denormalized identity (copied from SessionContext at write time).
    user_id         TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    -- Denormalized request metadata (same — copied at write time).
    trace_id        TEXT,
    priority        TEXT,
    model_id        TEXT,
    tool_name       TEXT,
    parent_msg_id   INTEGER REFERENCES session_messages(id) ON DELETE SET NULL,
    is_summarized   INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    UNIQUE(session_id, seq)
);

CREATE TABLE IF NOT EXISTS session_usage (
    session_id      TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    iterations      INTEGER NOT NULL DEFAULT 0,
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    cost_nano_usd   INTEGER NOT NULL DEFAULT 0,
    cost_complete   INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS session_turn_configs (
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_id         TEXT NOT NULL,
    config_revision INTEGER NOT NULL,
    effective_config TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    PRIMARY KEY (session_id, turn_id)
);

CREATE INDEX IF NOT EXISTS idx_messages_user
    ON session_messages(user_id, session_id);
CREATE INDEX IF NOT EXISTS idx_messages_agent
    ON session_messages(agent_id);
CREATE INDEX IF NOT EXISTS idx_messages_trace
    ON session_messages(trace_id) WHERE trace_id IS NOT NULL;

-- Boot filter: persistent + non-archived
CREATE INDEX IF NOT EXISTS idx_sessions_lifetime
    ON sessions(lifetime, is_archived);
CREATE INDEX IF NOT EXISTS idx_sessions_user
    ON sessions(user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_sessions_updated
    ON sessions(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_session_agents_agent
    ON session_agents(agent_id);
CREATE INDEX IF NOT EXISTS idx_messages_session
    ON session_messages(session_id, seq);
CREATE INDEX IF NOT EXISTS idx_messages_unsummarized
    ON session_messages(session_id, is_summarized);
";

fn ensure_usage_cost_columns(conn: &Connection) -> Result<(), SessionStoreError> {
    let has_column = |name: &str| -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('session_usage') WHERE name = ?1)",
            [name],
            |row| row.get(0),
        )
    };
    if !has_column("cost_nano_usd").map_err(sqlite_err)? {
        conn.execute(
            "ALTER TABLE session_usage ADD COLUMN cost_nano_usd INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(sqlite_err)?;
    }
    if !has_column("cost_complete").map_err(sqlite_err)? {
        // Existing usage predates pricing snapshots, so its full cost is unknown.
        conn.execute(
            "ALTER TABLE session_usage ADD COLUMN cost_complete INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(sqlite_err)?;
    }
    Ok(())
}

fn ensure_session_config_columns(conn: &Connection) -> Result<(), SessionStoreError> {
    let has_column = |name: &str| -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('sessions') WHERE name = ?1)",
            [name],
            |row| row.get(0),
        )
    };
    for (name, definition) in [
        ("config_revision", "INTEGER NOT NULL DEFAULT 0"),
        ("config_overrides", "TEXT NOT NULL DEFAULT '{}'"),
        ("effective_config", "TEXT"),
    ] {
        if !has_column(name).map_err(sqlite_err)? {
            conn.execute(
                &format!("ALTER TABLE sessions ADD COLUMN {name} {definition}"),
                [],
            )
            .map_err(sqlite_err)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionStore for SqliteSessionStore {
    // ---- session metadata CRUD ----

    async fn list_persistent(&self) -> Result<Vec<StoredSession>, SessionStoreError> {
        // Boot-loader path: returns all persistent, non-archived
        // sessions across all users. The caller (runtime::boot) is
        // itself a system-actor that creates AgentRuns per session;
        // per-user filtering happens in `list` at request time.
        self.run(|c| {
            let mut stmt = c.prepare(
                "SELECT s.id, s.name, s.lifetime, s.workspace, s.user_id, s.created_at, \
                        s.updated_at, s.external_meta, s.config_revision, s.config_overrides, \
                        s.effective_config, GROUP_CONCAT(sa.agent_id, ',') AS agents \
                 FROM sessions s \
                 LEFT JOIN session_agents sa ON sa.session_id = s.id \
                 WHERE s.lifetime = 'persistent' AND s.is_archived = 0 \
                 GROUP BY s.id \
                 ORDER BY s.updated_at DESC",
            )?;
            let rows = stmt.query_map([], row_to_session_with_agents)?;
            let mut out = Vec::new();
            for row in rows {
                let s = row?;
                out.push(s);
            }
            Ok(out)
        })
        .await
    }

    async fn save(&self, session: &StoredSession) -> Result<(), SessionStoreError> {
        let s = session.clone();
        self.run(move |c| {
            let external = serde_json::to_string(&s.external_meta)
                .map_err(|e| SessionStoreError::Store(format!("serialize external_meta: {e}")))?;
            let overrides = serde_json::to_string(&s.config_overrides).map_err(|error| {
                SessionStoreError::Store(format!("serialize session config overrides: {error}"))
            })?;
            let effective = s
                .effective_config
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| {
                    SessionStoreError::Store(format!("serialize effective config: {error}"))
                })?;
            let config_revision = i64::try_from(s.config_revision).map_err(|_| {
                SessionStoreError::Invalid("session config revision exceeds SQLite range".into())
            })?;
            let lifetime = match s.lifetime {
                SessionLifetime::Ephemeral => "ephemeral",
                SessionLifetime::Persistent => "persistent",
            };
            let workspace = s.metadata.workspace.to_string_lossy().to_string();
            let user_id = s.metadata.user_id.clone();
            let now = crate::session::now_secs();

            c.execute(
                "INSERT INTO sessions (id, name, lifetime, workspace, user_id, \
                                       created_at, updated_at, external_meta, config_revision, \
                                       config_overrides, effective_config, is_archived) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0) \
                 ON CONFLICT(id) DO UPDATE SET \
                   name = excluded.name, \
                   lifetime = excluded.lifetime, \
                   workspace = excluded.workspace, \
                   user_id = excluded.user_id, \
                   updated_at = excluded.updated_at, \
                   external_meta = excluded.external_meta, \
                   config_revision = excluded.config_revision, \
                   config_overrides = excluded.config_overrides, \
                   effective_config = excluded.effective_config",
                params![
                    s.id.0,
                    s.name,
                    lifetime,
                    workspace,
                    user_id,
                    s.created_at,
                    now,
                    external,
                    config_revision,
                    overrides,
                    effective,
                ],
            )?;

            // Refresh M:N agents (delete + reinsert is simplest; small N).
            c.execute(
                "DELETE FROM session_agents WHERE session_id = ?1",
                params![s.id.0],
            )?;
            for agent in &s.agents {
                c.execute(
                    "INSERT OR IGNORE INTO session_agents (session_id, agent_id, joined_at) \
                     VALUES (?1, ?2, ?3)",
                    params![s.id.0, agent.0, now],
                )?;
            }
            Ok(())
        })
        .await
    }

    async fn patch_metadata(
        &self,
        id: &SessionId,
        patch: SessionMetadataPatch,
    ) -> Result<(), SessionStoreError> {
        let id = id.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction().map_err(sqlite_err)?;
            let encoded: Option<String> = transaction
                .query_row(
                    "SELECT external_meta FROM sessions WHERE id = ?1 AND is_archived = 0",
                    params![id.0],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sqlite_err)?;
            let Some(encoded) = encoded else {
                return Err(SessionStoreError::NotFound(id));
            };
            let mut external_meta: std::collections::HashMap<String, JsonValue> =
                serde_json::from_str(&encoded).map_err(|error| {
                    SessionStoreError::Store(format!("deserialize external metadata: {error}"))
                })?;
            external_meta.extend(patch.external_meta);
            let encoded = serde_json::to_string(&external_meta).map_err(|error| {
                SessionStoreError::Store(format!("serialize external metadata: {error}"))
            })?;
            let updated = transaction
                .execute(
                    "UPDATE sessions SET name = COALESCE(?1, name), external_meta = ?2, \
                                         updated_at = ?3 \
                     WHERE id = ?4 AND is_archived = 0",
                    params![patch.name, encoded, crate::session::now_secs(), id.0],
                )
                .map_err(sqlite_err)?;
            if updated != 1 {
                return Err(SessionStoreError::NotFound(id));
            }
            transaction.commit().map_err(sqlite_err)
        })
        .await
    }

    async fn update_config(
        &self,
        id: &SessionId,
        expected_revision: u64,
        overrides: sylvander_protocol::SessionConfigOverrides,
        effective: sylvander_protocol::SessionEffectiveConfig,
    ) -> Result<u64, SessionStoreError> {
        let id = id.clone();
        let expected = i64::try_from(expected_revision).map_err(|_| {
            SessionStoreError::Invalid("expected config revision exceeds SQLite range".into())
        })?;
        let next = expected_revision
            .checked_add(1)
            .ok_or_else(|| SessionStoreError::Invalid("session config revision overflow".into()))?;
        let next_sql = i64::try_from(next).map_err(|_| {
            SessionStoreError::Invalid("new config revision exceeds SQLite range".into())
        })?;
        let overrides = serde_json::to_string(&overrides).map_err(|error| {
            SessionStoreError::Store(format!("serialize session config overrides: {error}"))
        })?;
        let effective = serde_json::to_string(&effective).map_err(|error| {
            SessionStoreError::Store(format!("serialize effective config: {error}"))
        })?;
        self.run(move |connection| {
            let updated = connection.execute(
                "UPDATE sessions SET config_revision = ?1, config_overrides = ?2, \
                                     effective_config = ?3, updated_at = ?4 \
                 WHERE id = ?5 AND is_archived = 0 AND config_revision = ?6",
                params![
                    next_sql,
                    overrides,
                    effective,
                    crate::session::now_secs(),
                    id.0,
                    expected,
                ],
            )?;
            if updated == 1 {
                return Ok(next);
            }
            let actual: Option<i64> = connection
                .query_row(
                    "SELECT config_revision FROM sessions WHERE id = ?1 AND is_archived = 0",
                    params![id.0],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(actual) = actual else {
                return Err(SessionStoreError::NotFound(id));
            };
            let actual = actual
                .try_into()
                .map_err(|_| SessionStoreError::Store("negative session config revision".into()))?;
            Err(SessionStoreError::ConfigConflict {
                expected: expected_revision,
                actual,
            })
        })
        .await
    }

    async fn begin_turn(
        &self,
        ctx: &sylvander_protocol::SessionContext,
        start: TurnStart,
    ) -> Result<StoredMessage, SessionStoreError> {
        if start.turn_id.trim().is_empty() {
            return Err(SessionStoreError::Invalid("turn id cannot be empty".into()));
        }
        let config_revision = i64::try_from(start.config_revision).map_err(|_| {
            SessionStoreError::Invalid("turn config revision exceeds SQLite range".into())
        })?;
        let effective_json = serde_json::to_string(&start.effective_config).map_err(|error| {
            SessionStoreError::Store(format!("serialize effective config: {error}"))
        })?;
        let content_json = serde_json::to_string(&start.user_content)
            .map_err(|error| SessionStoreError::Store(format!("serialize content: {error}")))?;
        let user_id = ctx.identity.user_id.0.clone();
        let agent_id = ctx.identity.agent_id.0.clone();
        let trace_id = ctx.request.trace_id.clone();
        let priority = Some(priority_str(ctx.request.priority));
        let stored_priority = Some(ctx.request.priority);
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction().map_err(sqlite_err)?;
            let stored: Option<(i64, Option<String>)> = transaction
                .query_row(
                    "SELECT config_revision, effective_config FROM sessions \
                     WHERE id = ?1 AND is_archived = 0",
                    params![start.session_id.0],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(sqlite_err)?;
            let Some((actual_revision, stored_effective)) = stored else {
                return Err(SessionStoreError::NotFound(start.session_id));
            };
            if actual_revision != config_revision {
                return Err(SessionStoreError::ConfigConflict {
                    expected: start.config_revision,
                    actual: actual_revision.try_into().map_err(|_| {
                        SessionStoreError::Store("negative session config revision".into())
                    })?,
                });
            }
            let stored_effective = stored_effective.ok_or_else(|| {
                SessionStoreError::Invalid("session effective configuration is unresolved".into())
            })?;
            let persisted: sylvander_protocol::SessionEffectiveConfig =
                decode_json(1, &stored_effective).map_err(sqlite_err)?;
            if persisted != start.effective_config {
                return Err(SessionStoreError::Invalid(
                    "turn configuration does not match the persisted session revision".into(),
                ));
            }

            let now = crate::session::now_secs();
            transaction
                .execute(
                    "INSERT INTO session_turn_configs \
                     (session_id, turn_id, config_revision, effective_config, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        start.session_id.0,
                        start.turn_id,
                        config_revision,
                        effective_json,
                        now,
                    ],
                )
                .map_err(sqlite_err)?;
            let next_seq: i64 = transaction
                .query_row(
                    "SELECT COALESCE(MAX(seq), -1) + 1 FROM session_messages \
                     WHERE session_id = ?1",
                    params![start.session_id.0],
                    |row| row.get(0),
                )
                .map_err(sqlite_err)?;
            transaction
                .execute(
                    "INSERT INTO session_messages \
                     (session_id, seq, role, content_json, user_id, agent_id, trace_id, priority, \
                      model_id, is_summarized, created_at) \
                     VALUES (?1, ?2, 'user', ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)",
                    params![
                        start.session_id.0,
                        next_seq,
                        content_json,
                        user_id,
                        agent_id,
                        trace_id,
                        priority,
                        start.model_id,
                        now,
                    ],
                )
                .map_err(sqlite_err)?;
            let message_id = transaction.last_insert_rowid();
            transaction
                .execute(
                    "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                    params![now, start.session_id.0],
                )
                .map_err(sqlite_err)?;
            transaction.commit().map_err(sqlite_err)?;
            Ok(StoredMessage {
                id: message_id,
                session_id: start.session_id,
                user_id: user_id.into(),
                agent_id: AgentId::new(agent_id),
                trace_id,
                priority: stored_priority,
                seq: next_seq.try_into().map_err(|_| {
                    SessionStoreError::Store("message sequence exceeds u32 range".into())
                })?,
                role: MessageRole::User,
                content: start.user_content,
                model_id: Some(start.model_id),
                tool_name: None,
                parent_msg_id: None,
                is_summarized: false,
                created_at: now,
            })
        })
        .await
    }

    async fn turn_config(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<Option<TurnConfigSnapshot>, SessionStoreError> {
        let session_id = session_id.clone();
        let turn_id = turn_id.to_string();
        self.run(move |connection| {
            connection
                .query_row(
                    "SELECT config_revision, effective_config, created_at \
                     FROM session_turn_configs WHERE session_id = ?1 AND turn_id = ?2",
                    params![session_id.0, turn_id],
                    |row| {
                        let config_revision: i64 = row.get(0)?;
                        let effective: String = row.get(1)?;
                        Ok(TurnConfigSnapshot {
                            session_id: session_id.clone(),
                            turn_id: turn_id.clone(),
                            config_revision: config_revision.try_into().map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    0,
                                    Type::Integer,
                                    Box::new(error),
                                )
                            })?,
                            effective_config: decode_json(1, &effective)?,
                            created_at: row.get(2)?,
                        })
                    },
                )
                .optional()
                .map_err(sqlite_err)
        })
        .await
    }

    async fn archive(&self, id: &SessionId) -> Result<(), SessionStoreError> {
        let id = id.clone();
        self.run(move |c| {
            let rows = c.execute(
                "UPDATE sessions SET is_archived = 1, \
                                       archive_reason = 'closed' \
                 WHERE id = ?1",
                params![id.0],
            )?;
            if rows == 0 {
                return Err(SessionStoreError::NotFound(id));
            }
            Ok(())
        })
        .await
    }

    async fn restore(&self, id: &SessionId) -> Result<(), SessionStoreError> {
        let id = id.clone();
        self.run(move |c| {
            let rows = c.execute(
                "UPDATE sessions SET is_archived = 0, archive_reason = NULL, updated_at = ?2 \
                 WHERE id = ?1 AND is_archived = 1",
                params![id.0, crate::session::now_secs()],
            )?;
            if rows == 0 {
                return Err(SessionStoreError::NotFound(id));
            }
            Ok(())
        })
        .await
    }

    async fn record_usage(
        &self,
        id: &SessionId,
        input_tokens: u32,
        output_tokens: u32,
        cost_nano_usd: Option<u64>,
    ) -> Result<SessionUsage, SessionStoreError> {
        let id = id.clone();
        self.run(move |c| {
            let stored_cost = i64::try_from(cost_nano_usd.unwrap_or(0)).map_err(|error| {
                SessionStoreError::Store(format!(
                    "usage cost exceeds SQLite INTEGER range: {error}"
                ))
            })?;
            c.execute(
                "INSERT INTO session_usage (session_id, iterations, input_tokens, output_tokens, cost_nano_usd, cost_complete) \
                 VALUES (?1, 1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(session_id) DO UPDATE SET \
                   iterations = iterations + 1, \
                   input_tokens = input_tokens + excluded.input_tokens, \
                   output_tokens = output_tokens + excluded.output_tokens, \
                   cost_nano_usd = cost_nano_usd + excluded.cost_nano_usd, \
                   cost_complete = cost_complete * excluded.cost_complete",
                params![
                    id.0,
                    input_tokens,
                    output_tokens,
                    stored_cost,
                    i64::from(cost_nano_usd.is_some())
                ],
            )?;
            read_usage(c, &id)
        })
        .await
    }

    async fn usage(&self, id: &SessionId) -> Result<SessionUsage, SessionStoreError> {
        let id = id.clone();
        self.run(move |c| read_usage(c, &id)).await
    }

    async fn delete(&self, id: &SessionId) -> Result<(), SessionStoreError> {
        let id = id.clone();
        self.run(move |c| {
            // ON DELETE CASCADE drops session_agents and session_messages.
            let rows = c.execute("DELETE FROM sessions WHERE id = ?1", params![id.0])?;
            if rows == 0 {
                return Err(SessionStoreError::NotFound(id));
            }
            Ok(())
        })
        .await
    }

    async fn get(&self, id: &SessionId) -> Result<Option<StoredSession>, SessionStoreError> {
        let id = id.clone();
        self.run(move |c| {
            // Combine session + agents in one query so we don't need
            // a second round-trip when fetching a single record.
            let mut stmt = c.prepare(
                "SELECT s.id, s.name, s.lifetime, s.workspace, s.user_id, \
                        s.created_at, s.updated_at, s.external_meta, s.config_revision, \
                        s.config_overrides, s.effective_config, \
                        GROUP_CONCAT(sa.agent_id, ',') AS agents \
                 FROM sessions s \
                 LEFT JOIN session_agents sa ON sa.session_id = s.id \
                 WHERE s.id = ?1 AND s.is_archived = 0 \
                 GROUP BY s.id",
            )?;
            let row = stmt
                .query_row(params![id.0], row_to_session_with_agents)
                .optional()?;
            Ok(row)
        })
        .await
    }

    async fn list(
        &self,
        ctx: &sylvander_protocol::SessionContext,
        filter: SessionFilter,
    ) -> Result<Vec<StoredSession>, SessionStoreError> {
        // Caller-scoping: a non-admin caller MUST set
        // `filter.identity = Some(caller.identity)`. We force that
        // here by injecting a WHERE user_id = ? into the query when
        // identity is Some. When None we return everything (admin).
        let caller_user = filter
            .identity
            .as_ref()
            .map_or_else(|| ctx.identity.user_id.0.clone(), |i| i.user_id.0.clone());
        let caller_agent = filter.identity.as_ref().map(|i| i.agent_id.0.clone());
        let force_scope = filter.identity.is_some();

        self.run(move |c| {
            let mut sql = String::from(
                "SELECT s.id, s.name, s.lifetime, s.workspace, s.user_id, \
                        s.created_at, s.updated_at, s.external_meta, s.config_revision, \
                        s.config_overrides, s.effective_config, \
                        GROUP_CONCAT(sa.agent_id, ',') AS agents \
                 FROM sessions s \
                 LEFT JOIN session_agents sa ON sa.session_id = s.id \
                 WHERE 1=1",
            );
            let mut bound: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

            if !filter.include_archived {
                sql.push_str(" AND s.is_archived = 0");
            }
            if force_scope {
                sql.push_str(" AND s.user_id = ?");
                bound.push(Box::new(caller_user.clone()));
            }
            if let Some(life) = filter.lifetime {
                sql.push_str(" AND s.lifetime = ?");
                bound.push(Box::new(
                    match life {
                        SessionLifetime::Ephemeral => "ephemeral",
                        SessionLifetime::Persistent => "persistent",
                    }
                    .to_string(),
                ));
            }
            if let Some(agent) = &caller_agent {
                sql.push_str(
                    " AND s.id IN (SELECT session_id FROM session_agents WHERE agent_id = ?)",
                );
                bound.push(Box::new(agent.clone()));
            }
            sql.push_str(" GROUP BY s.id ORDER BY s.updated_at DESC");
            if let Some(limit) = filter.limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }

            let mut stmt = c.prepare(&sql)?;
            let params_iter: Vec<&dyn rusqlite::ToSql> =
                bound.iter().map(|b| &**b as &dyn rusqlite::ToSql).collect();
            let rows = stmt.query_map(params_iter.as_slice(), row_to_session_with_agents)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .await
    }

    async fn search(
        &self,
        ctx: &sylvander_protocol::SessionContext,
        query: &str,
        limit: usize,
    ) -> Result<Vec<StoredSession>, SessionStoreError> {
        let query = query.to_string();
        // Scope to the caller's user_id; non-admins cannot see
        // other users' sessions even by guessing names.
        let scope_user = ctx.identity.user_id.0.clone();
        self.run(move |c| {
            // FTS5 would be wired here; for MVP we use LIKE %q%
            // on name + user_id, ordered by updated_at DESC.
            let pattern = format!("%{query}%");
            let mut stmt = c.prepare(
                "SELECT id, name, lifetime, workspace, user_id, created_at, updated_at, external_meta, \
                        config_revision, config_overrides, effective_config \
                 FROM sessions \
                 WHERE is_archived = 0 \
                   AND user_id = ?3 \
                   AND (name LIKE ?1 OR user_id LIKE ?1) \
                 ORDER BY updated_at DESC \
                 LIMIT ?2",
            )?;
            let limit = i64::try_from(limit).unwrap_or(i64::MAX);
            let rows = stmt.query_map(params![pattern, limit, scope_user], row_to_session_no_agents)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .await
    }

    // ---- message history ----

    async fn append_message(
        &self,
        ctx: &sylvander_protocol::SessionContext,
        session_id: &SessionId,
        role: MessageRole,
        content: JsonValue,
        model_id: Option<&str>,
        tool_name: Option<&str>,
        parent_msg_id: Option<i64>,
    ) -> Result<StoredMessage, SessionStoreError> {
        let session_id = session_id.clone();
        let role_str = match role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        let model_id = model_id.map(str::to_string);
        let tool_name = tool_name.map(str::to_string);
        let content_json = serde_json::to_string(&content)
            .map_err(|e| SessionStoreError::Store(format!("serialize content: {e}")))?;
        // Flatten the SessionContext into real columns. We do NOT
        // store it as a JSON blob — the API still takes the full
        // SessionContext (so call sites don't change), but storage
        // is denormalized for query efficiency.
        let user_id = ctx.identity.user_id.0.clone();
        let agent_id = ctx.identity.agent_id.0.clone();
        let trace_id = ctx.request.trace_id.clone();
        let priority = Some(priority_str(ctx.request.priority));
        let now = crate::session::now_secs();

        self.run(move |c| {
            // Verify session exists (and isn't archived) before insert.
            let exists: Option<i64> = c
                .query_row(
                    "SELECT 1 FROM sessions WHERE id = ?1 AND is_archived = 0",
                    params![session_id.0],
                    |r| r.get(0),
                )
                .optional()?;
            if exists.is_none() {
                return Err(SessionStoreError::NotFound(session_id.clone()));
            }

            // Compute next seq within the session. SQLite serializes
            // our access through the mutex, so MAX+1 is race-free here.
            let next_seq: i64 = c.query_row(
                "SELECT COALESCE(MAX(seq), -1) + 1 FROM session_messages \
                     WHERE session_id = ?1",
                params![session_id.0],
                |r| r.get(0),
            )?;

            c.execute(
                "INSERT INTO session_messages \
                 (session_id, seq, role, content_json, user_id, agent_id, \
                  trace_id, priority, model_id, tool_name, \
                  parent_msg_id, is_summarized, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12)",
                params![
                    session_id.0,
                    next_seq,
                    role_str,
                    content_json,
                    user_id,
                    agent_id,
                    trace_id,
                    priority,
                    model_id,
                    tool_name,
                    parent_msg_id,
                    now,
                ],
            )?;

            let id = c.last_insert_rowid();

            // Bump session.updated_at.
            c.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                params![now, session_id.0],
            )?;

            // Re-read to return the full StoredMessage.
            let row = c
                .query_row(
                    "SELECT id, session_id, seq, role, content_json, \
                            user_id, agent_id, trace_id, priority, \
                            model_id, tool_name, parent_msg_id, \
                            is_summarized, created_at \
                     FROM session_messages WHERE id = ?1",
                    params![id],
                    row_to_message,
                )
                .optional()?;
            row.ok_or_else(|| SessionStoreError::Store("just-inserted message vanished".into()))
        })
        .await
    }

    async fn read_history(
        &self,
        ctx: &sylvander_protocol::SessionContext,
        session_id: &SessionId,
        include_summarized: bool,
        limit: Option<usize>,
    ) -> Result<Vec<StoredMessage>, SessionStoreError> {
        let session_id = session_id.clone();
        let scope_user = ctx.identity.user_id.0.clone();
        self.run(move |c| {
            let mut sql = String::from(
                "SELECT id, session_id, seq, role, content_json, \
                        user_id, agent_id, trace_id, priority, \
                        model_id, tool_name, parent_msg_id, \
                        is_summarized, created_at \
                 FROM session_messages \
                 WHERE session_id = ?1 AND user_id = ?2",
            );
            if !include_summarized {
                sql.push_str(" AND is_summarized = 0");
            }
            sql.push_str(" ORDER BY seq ASC");
            if let Some(limit) = limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(params![session_id.0, scope_user], row_to_message)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .await
    }

    async fn mark_summarized(
        &self,
        session_id: &SessionId,
        seq_range: Range<u32>,
    ) -> Result<(), SessionStoreError> {
        let session_id = session_id.clone();
        self.run(move |c| {
            // Range is half-open: start inclusive, end exclusive.
            c.execute(
                "UPDATE session_messages SET is_summarized = 1 \
                 WHERE session_id = ?1 AND seq >= ?2 AND seq < ?3",
                params![session_id.0, seq_range.start, seq_range.end],
            )?;
            Ok(())
        })
        .await
    }

    async fn replace_active_history(
        &self,
        ctx: &sylvander_protocol::SessionContext,
        session_id: &SessionId,
        messages: Vec<ReplacementMessage>,
    ) -> Result<(), SessionStoreError> {
        if messages.is_empty() {
            return Err(SessionStoreError::Invalid(
                "replacement history cannot be empty".into(),
            ));
        }
        let session_id = session_id.clone();
        let user_id = ctx.identity.user_id.0.clone();
        let agent_id = ctx.identity.agent_id.0.clone();
        let trace_id = ctx.request.trace_id.clone();
        let priority = priority_str(ctx.request.priority);
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction().map_err(sqlite_err)?;
            let exists: Option<i64> = transaction
                .query_row(
                    "SELECT 1 FROM sessions WHERE id = ?1 AND is_archived = 0",
                    params![session_id.0],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sqlite_err)?;
            if exists.is_none() {
                return Err(SessionStoreError::NotFound(session_id));
            }
            transaction
                .execute(
                    "UPDATE session_messages SET is_summarized = 1 \
                     WHERE session_id = ?1 AND is_summarized = 0",
                    params![session_id.0],
                )
                .map_err(sqlite_err)?;
            let next_seq: i64 = transaction
                .query_row(
                    "SELECT COALESCE(MAX(seq), -1) + 1 FROM session_messages \
                     WHERE session_id = ?1",
                    params![session_id.0],
                    |row| row.get(0),
                )
                .map_err(sqlite_err)?;
            let now = crate::session::now_secs();
            for (next_seq, message) in (next_seq..).zip(messages) {
                let role = match message.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                };
                let content = serde_json::to_string(&message.content).map_err(|error| {
                    SessionStoreError::Store(format!("serialize replacement content: {error}"))
                })?;
                transaction
                    .execute(
                        "INSERT INTO session_messages \
                         (session_id, seq, role, content_json, user_id, agent_id, \
                          trace_id, priority, tool_name, is_summarized, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10)",
                        params![
                            session_id.0,
                            next_seq,
                            role,
                            content,
                            user_id,
                            agent_id,
                            trace_id,
                            priority,
                            message.tool_name,
                            now,
                        ],
                    )
                    .map_err(sqlite_err)?;
            }
            transaction
                .execute(
                    "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                    params![now, session_id.0],
                )
                .map_err(sqlite_err)?;
            transaction.commit().map_err(sqlite_err)
        })
        .await
    }

    async fn count_active_messages(
        &self,
        ctx: &sylvander_protocol::SessionContext,
        session_id: &SessionId,
    ) -> Result<u64, SessionStoreError> {
        let session_id = session_id.clone();
        let scope_user = ctx.identity.user_id.0.clone();
        self.run(move |c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM session_messages \
                 WHERE session_id = ?1 \
                   AND user_id = ?2 \
                   AND is_summarized = 0",
                params![session_id.0, scope_user],
                |r| r.get(0),
            )?;
            Ok(n as u64)
        })
        .await
    }
}

fn read_usage(c: &Connection, id: &SessionId) -> Result<SessionUsage, SessionStoreError> {
    Ok(c.query_row(
        "SELECT iterations, input_tokens, output_tokens, cost_nano_usd, cost_complete FROM session_usage WHERE session_id = ?1",
        params![id.0],
        |row| {
            let complete: bool = row.get(4)?;
            Ok(SessionUsage {
                iterations: row.get(0)?,
                input_tokens: read_nonnegative_u64(row, 1)?,
                output_tokens: read_nonnegative_u64(row, 2)?,
                cost_nano_usd: complete
                    .then(|| read_nonnegative_u64(row, 3))
                    .transpose()?,
            })
        },
    )
    .optional()?
    .unwrap_or_default())
}

fn read_nonnegative_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

// ---------------------------------------------------------------------------
// Row → struct helpers
// ---------------------------------------------------------------------------

/// Read a session row WITHOUT agent join (used by `list_persistent`
/// and `search` where we just want metadata; agents are filled in by
/// callers as needed).
fn row_to_session_no_agents(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredSession> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let lifetime: String = row.get(2)?;
    let workspace: String = row.get(3)?;
    let user_id: String = row.get(4)?;
    let created_at: i64 = row.get(5)?;
    let updated_at: i64 = row.get(6)?;
    let external: String = row.get(7)?;
    let config_revision: i64 = row.get(8)?;
    let config_overrides: String = row.get(9)?;
    let effective_config: Option<String> = row.get(10)?;

    Ok(StoredSession {
        id: SessionId::new(id),
        name,
        lifetime: parse_lifetime(&lifetime),
        metadata: SessionMetadata {
            workspace: std::path::PathBuf::from(workspace),
            name: String::new(),
            user_id,
        },
        agents: Vec::new(),
        created_at,
        updated_at,
        external_meta: decode_json(7, &external)?,
        config_revision: config_revision.try_into().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(8, Type::Integer, Box::new(error))
        })?,
        config_overrides: decode_json(9, &config_overrides)?,
        effective_config: effective_config
            .as_deref()
            .map(|value| decode_json(10, value))
            .transpose()?,
    })
}

/// Read a session row WITH agent join. `agents` is a comma-separated
/// string from `GROUP_CONCAT` (NULL when no agents).
fn row_to_session_with_agents(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredSession> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let lifetime: String = row.get(2)?;
    let workspace: String = row.get(3)?;
    let user_id: String = row.get(4)?;
    let created_at: i64 = row.get(5)?;
    let updated_at: i64 = row.get(6)?;
    let external: String = row.get(7)?;
    let config_revision: i64 = row.get(8)?;
    let config_overrides: String = row.get(9)?;
    let effective_config: Option<String> = row.get(10)?;
    let agents_csv: Option<String> = row.get(11)?;

    let agents = agents_csv
        .map(|s| {
            s.split(',')
                .filter(|s| !s.is_empty())
                .map(|s| AgentId::new(s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(StoredSession {
        id: SessionId::new(id),
        name,
        lifetime: parse_lifetime(&lifetime),
        metadata: SessionMetadata {
            workspace: std::path::PathBuf::from(workspace),
            name: String::new(),
            user_id,
        },
        agents,
        created_at,
        updated_at,
        external_meta: decode_json(7, &external)?,
        config_revision: config_revision.try_into().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(8, Type::Integer, Box::new(error))
        })?,
        config_overrides: decode_json(9, &config_overrides)?,
        effective_config: effective_config
            .as_deref()
            .map(|value| decode_json(10, value))
            .transpose()?,
    })
}

fn decode_json<T: DeserializeOwned>(index: usize, value: &str) -> rusqlite::Result<T> {
    serde_json::from_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
    })
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    let id: i64 = row.get(0)?;
    let session_id: String = row.get(1)?;
    let seq: i64 = row.get(2)?;
    let role: String = row.get(3)?;
    let content_json: String = row.get(4)?;
    let user_id: String = row.get(5)?;
    let agent_id: String = row.get(6)?;
    let trace_id: Option<String> = row.get(7)?;
    let priority: Option<String> = row.get(8)?;
    let model_id: Option<String> = row.get(9)?;
    let tool_name: Option<String> = row.get(10)?;
    let parent_msg_id: Option<i64> = row.get(11)?;
    let is_summarized: i64 = row.get(12)?;
    let created_at: i64 = row.get(13)?;

    let role = match role.as_str() {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "tool" => MessageRole::Tool,
        other => {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(format!("unknown message role: {other}")),
            ));
        }
    };

    let content = serde_json::from_str(&content_json).map_err(|e| {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!("message content parse: {e}")),
        )
    })?;

    let priority = priority.as_deref().map(parse_priority).transpose()?;

    Ok(StoredMessage {
        id,
        session_id: SessionId::new(session_id),
        user_id: sylvander_protocol::types::UserId::new(user_id),
        agent_id: sylvander_protocol::types::AgentId::new(agent_id),
        trace_id,
        priority,
        seq: u32::try_from(seq).unwrap_or(u32::MAX),
        role,
        content,
        model_id,
        tool_name,
        parent_msg_id,
        is_summarized: is_summarized != 0,
        created_at,
    })
}

fn parse_lifetime(s: &str) -> SessionLifetime {
    match s {
        "persistent" => SessionLifetime::Persistent,
        _ => SessionLifetime::Ephemeral,
    }
}

// ---------------------------------------------------------------------------
// Error conversions
// ---------------------------------------------------------------------------

fn sqlite_err(e: rusqlite::Error) -> SessionStoreError {
    SessionStoreError::Store(e.to_string())
}
// ---------------------------------------------------------------------------
// Priority <-> str
// ---------------------------------------------------------------------------

fn priority_str(p: sylvander_protocol::session_context::Priority) -> String {
    use sylvander_protocol::session_context::Priority;
    match p {
        Priority::Low => "low",
        Priority::Normal => "normal",
        Priority::High => "high",
        Priority::Urgent => "urgent",
    }
    .to_string()
}

fn parse_priority(s: &str) -> rusqlite::Result<sylvander_protocol::session_context::Priority> {
    use sylvander_protocol::session_context::Priority;
    Ok(match s {
        "low" => Priority::Low,
        "normal" => Priority::Normal,
        "high" => Priority::High,
        "urgent" => Priority::Urgent,
        other => {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(format!("unknown priority: {other}")),
            ));
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../../tests/unit/session_store_sqlite.rs"]
mod tests;
