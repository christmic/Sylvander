//! Session persistence — SQLite backend.
//!
//! Two tables:
//! - `sessions`         — session metadata (id, name, lifetime, agents, ...)
//! - `session_messages` — every user / assistant / tool message, ordered
//!
//! Plus `session_agents` M:N join and `sessions_fts` FTS5 virtual table
//! for full-text search over session names.
//!
//! M3 L4 summarization integrates via the `is_summarized` flag on
//! messages: compressed messages stay on disk but are excluded from
//! `read_history(include_summarized=false)` — the loop's view.

mod sqlite;

pub use sqlite::SqliteSessionStore;

use std::collections::HashMap;
use std::ops::Range;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::session::SessionMetadata;
use crate::spec::{AgentId, SessionId};

// ---------------------------------------------------------------------------
// SessionLifetime
// ---------------------------------------------------------------------------

/// Whether a session survives system restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifetime {
    /// Created per conversation, destroyed when done.
    Ephemeral,
    /// Long-lived — group chats, channels, etc.
    Persistent,
}

// ---------------------------------------------------------------------------
// StoredSession
// ---------------------------------------------------------------------------

/// A session record in the persistence layer.
///
/// More complete than `SessionMeta` — includes protocol metadata
/// and lifetime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub id: SessionId,
    pub name: String,
    pub lifetime: SessionLifetime,
    pub metadata: SessionMetadata,
    pub agents: Vec<AgentId>,
    pub created_at: i64,
    /// Protocol-specific metadata (agent never sees this).
    #[serde(default)]
    pub external_meta: HashMap<String, JsonValue>,
}

impl StoredSession {
    /// Create a new stored session record.
    #[must_use]
    pub fn new(
        id: SessionId,
        name: impl Into<String>,
        lifetime: SessionLifetime,
        metadata: SessionMetadata,
        agents: Vec<AgentId>,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            lifetime,
            metadata,
            agents,
            created_at: crate::session::now_secs(),
            external_meta: HashMap::new(),
        }
    }

    /// Attach protocol-specific metadata.
    #[must_use]
    pub fn with_external_meta(
        mut self,
        key: impl Into<String>,
        value: impl Into<JsonValue>,
    ) -> Self {
        self.external_meta.insert(key.into(), value.into());
        self
    }
}

// ---------------------------------------------------------------------------
// MessageRole + StoredMessage
// ---------------------------------------------------------------------------

/// Role of a message in a session conversation.
///
/// - `User`:      a human / external actor's message
/// - `Assistant`: the agent's reply (may contain tool_use blocks)
/// - `Tool`:      the result of a tool call (parent_msg_id points to the
///                assistant message that issued the tool_use)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    Tool,
}

/// One persisted message.
///
/// `content` is wire-format JSON matching Anthropic's `MessageParam` /
/// `Message` shape, so the stored history can be fed back into a new
/// `AgentLoop::run` call after a restart without re-serialization.
///
/// Storage layout (SQLite `session_messages`):
/// - `seq` is auto-assigned (next integer in session).
/// - `id` is the SQLite rowid (auto-increment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: i64,
    pub session_id: SessionId,
    pub seq: u32,
    pub role: MessageRole,
    /// Wire-format JSON. Matches Anthropic's `MessageParam` shape:
    /// - user:      `{"role":"user","content":"hi"}` or `{"role":"user","content":[...]}`
    /// - assistant: `{"role":"assistant","content":[TextBlock|ToolUseBlock|...]}`
    /// - tool:      `{"role":"user","content":[{"type":"tool_result",...}]}`
    pub content: JsonValue,
    pub model_id: Option<String>,
    pub tool_name: Option<String>,
    pub parent_msg_id: Option<i64>,
    /// True once M3 L4 summarization has folded this message into a
    /// summary. Excluded from `read_history(include_summarized=false)`.
    pub is_summarized: bool,
    pub created_at: i64,
}

// ---------------------------------------------------------------------------
// SessionFilter
// ---------------------------------------------------------------------------

/// Filter for `SessionStore::list`. All set fields AND together;
/// `None` = wildcard.
#[derive(Debug, Default, Clone)]
pub struct SessionFilter {
    pub user_id: Option<String>,
    pub agent_id: Option<AgentId>,
    pub lifetime: Option<SessionLifetime>,
    /// When false (default), archived sessions are hidden.
    pub include_archived: bool,
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// SessionStore trait
// ---------------------------------------------------------------------------

/// Persistence backend for sessions + their message history.
///
/// Only one implementation is shipped today: `SqliteSessionStore`.
/// The trait stays so callers can mock in tests if needed.
#[async_trait]
pub trait SessionStore: Send + Sync {
    // ---- session metadata CRUD ----

    /// List persistent, non-archived sessions (boot loader).
    async fn list_persistent(&self) -> Result<Vec<StoredSession>, SessionStoreError>;

    /// Save or update a session record (upsert).
    async fn save(&self, session: &StoredSession) -> Result<(), SessionStoreError>;

    /// Soft-delete (sets `is_archived=1`). The row and its messages
    /// remain on disk for audit / undo; `get` returns `None`.
    async fn archive(&self, id: &SessionId) -> Result<(), SessionStoreError>;

    /// Hard-delete. Cascades through `session_agents` and
    /// `session_messages`. Use only on explicit user action.
    async fn delete(&self, id: &SessionId) -> Result<(), SessionStoreError>;

    /// Look up a session by ID.
    async fn get(
        &self,
        id: &SessionId,
    ) -> Result<Option<StoredSession>, SessionStoreError>;

    /// List sessions matching a filter. Used by runtime to scope by
    /// user / agent / lifetime without loading everything.
    async fn list(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<StoredSession>, SessionStoreError>;

    /// Full-text search over session name + user_id via SQLite FTS5.
    /// Returns matches ordered by relevance, capped at `limit`.
    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<StoredSession>, SessionStoreError>;

    // ---- message history ----

    /// Append a message to a session's history. `seq` is auto-assigned
    /// (next integer in session). Returns the stored record (with
    /// `id` and assigned `seq`).
    async fn append_message(
        &self,
        session_id: &SessionId,
        role: MessageRole,
        content: JsonValue,
        model_id: Option<&str>,
        tool_name: Option<&str>,
        parent_msg_id: Option<i64>,
    ) -> Result<StoredMessage, SessionStoreError>;

    /// Read all messages for a session, ordered by `seq` ascending.
    /// `include_summarized=false` skips M3 L4-compacted messages.
    /// `limit` caps the result (most recent N if Some).
    async fn read_history(
        &self,
        session_id: &SessionId,
        include_summarized: bool,
        limit: Option<usize>,
    ) -> Result<Vec<StoredMessage>, SessionStoreError>;

    /// Mark a contiguous range of messages as summarized.
    /// Called by M3 L4 when it produces a summary message that
    /// supersedes older ones.
    async fn mark_summarized(
        &self,
        session_id: &SessionId,
        seq_range: Range<u32>,
    ) -> Result<(), SessionStoreError>;

    /// Count non-summarized messages. Cheap O(1) on SQLite.
    async fn count_active_messages(
        &self,
        session_id: &SessionId,
    ) -> Result<u64, SessionStoreError>;
}

// ---------------------------------------------------------------------------
// SessionStoreError
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("store error: {0}")]
    Store(String),
    #[error("session not found: {0}")]
    NotFound(SessionId),
    #[error("invalid argument: {0}")]
    Invalid(String),
}

impl From<rusqlite::Error> for SessionStoreError {
    fn from(e: rusqlite::Error) -> Self {
        SessionStoreError::Store(e.to_string())
    }
}