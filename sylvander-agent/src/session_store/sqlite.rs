//! SQLite-backed [`SessionStore`].
//!
//! MVP concurrency model: a single `rusqlite::Connection` guarded by
//! `tokio::sync::Mutex`. All calls go through `spawn_blocking` so
//! SQLite work never stalls the async runtime. Adequate for desktop
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
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as JsonValue;
use tokio::sync::Mutex;
use tokio::task;

use crate::session::SessionMetadata;
use crate::spec::{AgentId, SessionId};

use super::{
    MessageRole, SessionFilter, SessionLifetime, SessionStore, SessionStoreError,
    StoredMessage, StoredSession,
};

/// SQLite-backed session store.
#[derive(Clone)]
pub struct SqliteSessionStore {
    inner: Arc<StoreInner>,
}

struct StoreInner {
    /// Synchronous SQLite connection. Guarded by `Mutex` so async tasks
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

    /// In-memory SQLite (`:memory:`). Used in tests; supports the
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

const SCHEMA_SQL: &str = r#"
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
CREATE TABLE IF NOT EXISTS session_messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    role            TEXT NOT NULL,
    content_json    TEXT NOT NULL,
    model_id        TEXT,
    tool_name       TEXT,
    parent_msg_id   INTEGER REFERENCES session_messages(id) ON DELETE SET NULL,
    is_summarized   INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    UNIQUE(session_id, seq)
);

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
"#;

// ---------------------------------------------------------------------------
// Trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionStore for SqliteSessionStore {
    // ---- session metadata CRUD ----

    async fn list_persistent(&self) -> Result<Vec<StoredSession>, SessionStoreError> {
        self.run(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, lifetime, workspace, user_id, created_at, external_meta \
                 FROM sessions \
                 WHERE lifetime = 'persistent' AND is_archived = 0 \
                 ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map([], row_to_session_no_agents)?;
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
            let lifetime = match s.lifetime {
                SessionLifetime::Ephemeral => "ephemeral",
                SessionLifetime::Persistent => "persistent",
            };
            let workspace = s.metadata.workspace.to_string_lossy().to_string();
            let user_id = s.metadata.user_id.clone();
            let now = crate::session::now_secs();

            c.execute(
                "INSERT INTO sessions (id, name, lifetime, workspace, user_id, \
                                       created_at, updated_at, external_meta, is_archived) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0) \
                 ON CONFLICT(id) DO UPDATE SET \
                   name = excluded.name, \
                   lifetime = excluded.lifetime, \
                   workspace = excluded.workspace, \
                   user_id = excluded.user_id, \
                   updated_at = excluded.updated_at, \
                   external_meta = excluded.external_meta",
                params![
                    s.id.0,
                    s.name,
                    lifetime,
                    workspace,
                    user_id,
                    s.created_at,
                    now,
                    external,
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
                return Err(SessionStoreError::NotFound(id).into());
            }
            Ok(())
        })
        .await
    }

    async fn delete(&self, id: &SessionId) -> Result<(), SessionStoreError> {
        let id = id.clone();
        self.run(move |c| {
            // ON DELETE CASCADE drops session_agents and session_messages.
            let rows = c.execute("DELETE FROM sessions WHERE id = ?1", params![id.0])?;
            if rows == 0 {
                return Err(SessionStoreError::NotFound(id).into());
            }
            Ok(())
        })
        .await
    }

    async fn get(
        &self,
        id: &SessionId,
    ) -> Result<Option<StoredSession>, SessionStoreError> {
        let id = id.clone();
        self.run(move |c| {
            // Combine session + agents in one query so we don't need
            // a second round-trip when fetching a single record.
            let mut stmt = c.prepare(
                "SELECT s.id, s.name, s.lifetime, s.workspace, s.user_id, \
                        s.created_at, s.external_meta, \
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
        filter: SessionFilter,
    ) -> Result<Vec<StoredSession>, SessionStoreError> {
        self.run(move |c| {
            let mut sql = String::from(
                "SELECT s.id, s.name, s.lifetime, s.workspace, s.user_id, \
                        s.created_at, s.external_meta, \
                        GROUP_CONCAT(sa.agent_id, ',') AS agents \
                 FROM sessions s \
                 LEFT JOIN session_agents sa ON sa.session_id = s.id \
                 WHERE 1=1",
            );
            let mut bound: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

            if !filter.include_archived {
                sql.push_str(" AND s.is_archived = 0");
            }
            if let Some(uid) = &filter.user_id {
                sql.push_str(" AND s.user_id = ?");
                bound.push(Box::new(uid.clone()));
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
            if let Some(agent) = &filter.agent_id {
                sql.push_str(
                    " AND s.id IN (SELECT session_id FROM session_agents WHERE agent_id = ?)",
                );
                bound.push(Box::new(agent.0.clone()));
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
        query: &str,
        limit: usize,
    ) -> Result<Vec<StoredSession>, SessionStoreError> {
        let query = query.to_string();
        self.run(move |c| {
            // FTS5 would be wired here; for MVP we use LIKE %q%
            // on name + user_id, ordered by updated_at DESC.
            let pattern = format!("%{query}%");
            let mut stmt = c.prepare(
                "SELECT id, name, lifetime, workspace, user_id, created_at, external_meta \
                 FROM sessions \
                 WHERE is_archived = 0 \
                   AND (name LIKE ?1 OR user_id LIKE ?1) \
                 ORDER BY updated_at DESC \
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(
                params![pattern, limit as i64],
                row_to_session_no_agents,
            )?;
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
        let content_json = serde_json::to_string(&content).map_err(|e| {
            SessionStoreError::Store(format!("serialize content: {e}"))
        })?;
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
                return Err(SessionStoreError::NotFound(session_id.clone()).into());
            }

            // Compute next seq within the session. SQLite serializes
            // our access through the mutex, so MAX+1 is race-free here.
            let next_seq: i64 = c
                .query_row(
                    "SELECT COALESCE(MAX(seq), -1) + 1 FROM session_messages \
                     WHERE session_id = ?1",
                    params![session_id.0],
                    |r| r.get(0),
                )?;

            c.execute(
                "INSERT INTO session_messages \
                 (session_id, seq, role, content_json, model_id, tool_name, \
                  parent_msg_id, is_summarized, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)",
                params![
                    session_id.0,
                    next_seq,
                    role_str,
                    content_json,
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
                    "SELECT id, session_id, seq, role, content_json, model_id, \
                            tool_name, parent_msg_id, is_summarized, created_at \
                     FROM session_messages WHERE id = ?1",
                    params![id],
                    row_to_message,
                )
                .optional()?;
            row.ok_or_else(|| {
                SessionStoreError::Store("just-inserted message vanished".into())
            })
        })
        .await
    }

    async fn read_history(
        &self,
        session_id: &SessionId,
        include_summarized: bool,
        limit: Option<usize>,
    ) -> Result<Vec<StoredMessage>, SessionStoreError> {
        let session_id = session_id.clone();
        self.run(move |c| {
            let mut sql = String::from(
                "SELECT id, session_id, seq, role, content_json, model_id, \
                        tool_name, parent_msg_id, is_summarized, created_at \
                 FROM session_messages WHERE session_id = ?1",
            );
            if !include_summarized {
                sql.push_str(" AND is_summarized = 0");
            }
            sql.push_str(" ORDER BY seq ASC");
            if let Some(limit) = limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(params![session_id.0], row_to_message)?;
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

    async fn count_active_messages(
        &self,
        session_id: &SessionId,
    ) -> Result<u64, SessionStoreError> {
        let session_id = session_id.clone();
        self.run(move |c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM session_messages \
                 WHERE session_id = ?1 AND is_summarized = 0",
                params![session_id.0],
                |r| r.get(0),
            )?;
            Ok(n as u64)
        })
        .await
    }
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
    let external: String = row.get(6)?;

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
        external_meta: serde_json::from_str(&external).unwrap_or_default(),
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
    let external: String = row.get(6)?;
    let agents_csv: Option<String> = row.get(7)?;

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
        external_meta: serde_json::from_str(&external).unwrap_or_default(),
    })
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    let id: i64 = row.get(0)?;
    let session_id: String = row.get(1)?;
    let seq: i64 = row.get(2)?;
    let role: String = row.get(3)?;
    let content_json: String = row.get(4)?;
    let model_id: Option<String> = row.get(5)?;
    let tool_name: Option<String> = row.get(6)?;
    let parent_msg_id: Option<i64> = row.get(7)?;
    let is_summarized: i64 = row.get(8)?;
    let created_at: i64 = row.get(9)?;

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

    Ok(StoredMessage {
        id,
        session_id: SessionId::new(session_id),
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_meta() -> SessionMetadata {
        SessionMetadata {
            workspace: PathBuf::from("/tmp"),
            name: "test".into(),
            user_id: "user-1".into(),
        }
    }

    fn make_session(id: &str, lifetime: SessionLifetime) -> StoredSession {
        StoredSession::new(
            SessionId::new(id),
            format!("session-{id}"),
            lifetime,
            test_meta(),
            vec![AgentId::new("agent-1")],
        )
    }

    // ---- session metadata ----

    #[tokio::test]
    async fn list_persistent_filters_correctly() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        store.save(&make_session("s1", SessionLifetime::Persistent)).await.unwrap();
        store.save(&make_session("s2", SessionLifetime::Ephemeral)).await.unwrap();

        let persistent = store.list_persistent().await.unwrap();
        assert_eq!(persistent.len(), 1);
        assert_eq!(persistent[0].id, SessionId::new("s1"));
    }

    #[tokio::test]
    async fn save_and_get() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        store.save(&make_session("s1", SessionLifetime::Persistent)).await.unwrap();

        let found = store.get(&SessionId::new("s1")).await.unwrap();
        assert!(found.is_some());
        let s = found.unwrap();
        assert_eq!(s.agents.len(), 1);
        assert_eq!(s.agents[0], AgentId::new("agent-1"));
    }

    #[tokio::test]
    async fn save_is_upsert() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        store.save(&make_session("s1", SessionLifetime::Persistent)).await.unwrap();
        // Save again with a new name — should update, not duplicate.
        let mut updated = make_session("s1", SessionLifetime::Persistent);
        updated.name = "renamed".into();
        store.save(&updated).await.unwrap();

        let all = store.list_persistent().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "renamed");
    }

    #[tokio::test]
    async fn delete_removes() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        store.save(&make_session("s1", SessionLifetime::Ephemeral)).await.unwrap();
        store.delete(&SessionId::new("s1")).await.unwrap();
        assert!(store.get(&SessionId::new("s1")).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn archive_soft_deletes() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        store.save(&make_session("s1", SessionLifetime::Persistent)).await.unwrap();
        store.archive(&SessionId::new("s1")).await.unwrap();

        // get returns None (treats archived as gone from active set)
        assert!(store.get(&SessionId::new("s1")).await.unwrap().is_none());

        // list_persistent hides archived
        let visible = store.list_persistent().await.unwrap();
        assert!(visible.iter().all(|s| s.id != SessionId::new("s1")));

        // list with include_archived=true brings it back
        let filter = SessionFilter { include_archived: true, ..Default::default() };
        let all = store.list(filter).await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn list_filters_by_user() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();

        let mut s_a = make_session("s-a", SessionLifetime::Persistent);
        s_a.metadata.user_id = "alice".into();
        let mut s_b = make_session("s-b", SessionLifetime::Persistent);
        s_b.metadata.user_id = "bob".into();

        store.save(&s_a).await.unwrap();
        store.save(&s_b).await.unwrap();

        let filter = SessionFilter { user_id: Some("alice".into()), ..Default::default() };
        let alice_sessions = store.list(filter).await.unwrap();
        assert_eq!(alice_sessions.len(), 1);
        assert_eq!(alice_sessions[0].id, SessionId::new("s-a"));
    }

    #[tokio::test]
    async fn search_finds_by_name_substring() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        let mut s1 = make_session("s1", SessionLifetime::Persistent);
        s1.name = "修复登录 bug".into();
        let mut s2 = make_session("s2", SessionLifetime::Persistent);
        s2.name = "重构 API".into();
        store.save(&s1).await.unwrap();
        store.save(&s2).await.unwrap();

        let hits = store.search("登录", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, SessionId::new("s1"));
    }

    // ---- messages ----

    #[tokio::test]
    async fn append_message_assigns_seq() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        store.save(&make_session("s1", SessionLifetime::Persistent)).await.unwrap();

        let m1 = store
            .append_message(
                &SessionId::new("s1"),
                MessageRole::User,
                serde_json::json!({"role":"user","content":"hi"}),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let m2 = store
            .append_message(
                &SessionId::new("s1"),
                MessageRole::Assistant,
                serde_json::json!({"role":"assistant","content":[{"type":"text","text":"hello"}]}),
                Some("claude-sonnet-5"),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(m1.seq, 0);
        assert_eq!(m2.seq, 1);
        assert_eq!(m2.model_id.as_deref(), Some("claude-sonnet-5"));
    }

    #[tokio::test]
    async fn read_history_returns_in_order() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        store.save(&make_session("s1", SessionLifetime::Persistent)).await.unwrap();

        for i in 0..3 {
            store
                .append_message(
                    &SessionId::new("s1"),
                    MessageRole::User,
                    serde_json::json!({"i": i}),
                    None,
                    None,
                    None,
                )
                .await
                .unwrap();
        }

        let history = store.read_history(&SessionId::new("s1"), false, None).await.unwrap();
        assert_eq!(history.len(), 3);
        for (i, m) in history.iter().enumerate() {
            assert_eq!(m.seq, i as u32);
        }
    }

    #[tokio::test]
    async fn read_history_excludes_summarized() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        store.save(&make_session("s1", SessionLifetime::Persistent)).await.unwrap();
        for i in 0..3 {
            store
                .append_message(
                    &SessionId::new("s1"),
                    MessageRole::User,
                    serde_json::json!({"i": i}),
                    None,
                    None,
                    None,
                )
                .await
                .unwrap();
        }
        // Mark seq 0..2 (i.e. seq 0 and 1) as summarized.
        store.mark_summarized(&SessionId::new("s1"), 0..2).await.unwrap();

        let active = store.read_history(&SessionId::new("s1"), false, None).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].seq, 2);

        let all = store.read_history(&SessionId::new("s1"), true, None).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn count_active_messages() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        store.save(&make_session("s1", SessionLifetime::Persistent)).await.unwrap();
        for _ in 0..5 {
            store
                .append_message(
                    &SessionId::new("s1"),
                    MessageRole::User,
                    serde_json::json!({}),
                    None,
                    None,
                    None,
                )
                .await
                .unwrap();
        }
        store.mark_summarized(&SessionId::new("s1"), 0..3).await.unwrap();

        assert_eq!(store.count_active_messages(&SessionId::new("s1")).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn cascade_delete_drops_messages() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        store.save(&make_session("s1", SessionLifetime::Persistent)).await.unwrap();
        store
            .append_message(
                &SessionId::new("s1"),
                MessageRole::User,
                serde_json::json!({}),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        store.delete(&SessionId::new("s1")).await.unwrap();

        // The message row is gone (CASCADE).
        let history = store.read_history(&SessionId::new("s1"), true, None).await.unwrap();
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn append_to_missing_session_errors() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        let result = store
            .append_message(
                &SessionId::new("nonexistent"),
                MessageRole::User,
                serde_json::json!({}),
                None,
                None,
                None,
            )
            .await;
        assert!(matches!(result, Err(SessionStoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn concurrent_saves_serialize_safely() {
        let store = SqliteSessionStore::open_in_memory().await.unwrap();
        store.save(&make_session("s1", SessionLifetime::Persistent)).await.unwrap();

        // Spawn 10 concurrent appends — must not deadlock or panic.
        let mut handles = Vec::new();
        for i in 0..10 {
            let s = store.clone();
            handles.push(tokio::spawn(async move {
                s.append_message(
                    &SessionId::new("s1"),
                    MessageRole::User,
                    serde_json::json!({"i": i}),
                    None,
                    None,
                    None,
                )
                .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }

        let count = store.count_active_messages(&SessionId::new("s1")).await.unwrap();
        assert_eq!(count, 10);

        // All seq values must be unique and contiguous.
        let history = store.read_history(&SessionId::new("s1"), false, None).await.unwrap();
        let seqs: Vec<u32> = history.iter().map(|m| m.seq).collect();
        let mut sorted = seqs.clone();
        sorted.sort();
        assert_eq!(seqs, sorted, "seqs must be assigned uniquely");
    }

    #[tokio::test]
    async fn file_backed_store_persists_across_opens() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("sessions.db");

        // Write one session and message.
        let s1 = SqliteSessionStore::open(&path).await.unwrap();
        s1.save(&make_session("p1", SessionLifetime::Persistent)).await.unwrap();
        s1.append_message(
            &SessionId::new("p1"),
            MessageRole::User,
            serde_json::json!({"hello": "world"}),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        drop(s1);

        // Reopen — data should still be there.
        let s2 = SqliteSessionStore::open(&path).await.unwrap();
        let found = s2.get(&SessionId::new("p1")).await.unwrap();
        assert!(found.is_some());
        let history = s2.read_history(&SessionId::new("p1"), false, None).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content["hello"], "world");
    }
}