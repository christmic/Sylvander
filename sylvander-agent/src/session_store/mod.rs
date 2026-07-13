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
use sylvander_protocol::types::UserId;

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
    /// Last metadata/message activity, used for reliable recency ordering.
    #[serde(default)]
    pub updated_at: i64,
    /// Protocol-specific metadata (agent never sees this).
    #[serde(default)]
    pub external_meta: HashMap<String, JsonValue>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUsage {
    pub iterations: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
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
        let now = crate::session::now_secs();
        Self {
            id,
            name: name.into(),
            lifetime,
            metadata,
            agents,
            created_at: now,
            updated_at: now,
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
///
/// Identity / trace / priority are denormalized as real columns
/// (not stored as a JSON blob) so SQLite can use indexes for
/// per-user / per-trace lookups. They are written at `append_message`
/// time from the caller's `SessionContext`; readers reconstruct a
/// `SessionContext` if they need one. Adding a new `SessionContext`
/// field means `ALTER TABLE ADD COLUMN`, not editing a json blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: i64,
    /// The session this message belongs to.
    pub session_id: SessionId,
    /// Denormalized from `SessionContext::identity.user_id` at write
    /// time. Storing it as a real column (not nested in a JSON
    /// blob) lets us index and query per-user efficiently.
    pub user_id: UserId,
    /// Denormalized from `SessionContext::identity.agent_id`.
    pub agent_id: AgentId,
    /// Denormalized from `SessionContext::request.trace_id` (if set).
    pub trace_id: Option<String>,
    /// Denormalized from `SessionContext::request.priority`.
    pub priority: Option<sylvander_protocol::session_context::Priority>,
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

/// One message in an atomic active-history replacement.
#[derive(Debug, Clone)]
pub struct ReplacementMessage {
    pub role: MessageRole,
    pub content: JsonValue,
    pub tool_name: Option<String>,
}

// ---------------------------------------------------------------------------
// SessionFilter
// ---------------------------------------------------------------------------

/// Filter for `SessionStore::list`. All set fields AND together;
/// `None` = wildcard.
///
/// Use `identity` to scope by user / agent / session instead of
/// scattered `user_id` / `agent_id` fields. New identity fields
/// added to `SessionContext` will be honored by the implementation
/// without changing this struct.
#[derive(Debug, Default, Clone)]
pub struct SessionFilter {
    /// Scope to a specific identity. `None` = all identities (admin
    /// path). Caller must check authorization before passing `None`.
    pub identity: Option<sylvander_protocol::Identity>,
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

    /// Undo a soft-delete. The session and its messages become visible again.
    async fn restore(&self, id: &SessionId) -> Result<(), SessionStoreError>;

    /// Atomically add one model iteration to durable session accounting.
    async fn record_usage(
        &self,
        id: &SessionId,
        input_tokens: u32,
        output_tokens: u32,
    ) -> Result<SessionUsage, SessionStoreError>;

    async fn usage(&self, id: &SessionId) -> Result<SessionUsage, SessionStoreError>;

    /// Hard-delete. Cascades through `session_agents` and
    /// `session_messages`. Use only on explicit user action.
    async fn delete(&self, id: &SessionId) -> Result<(), SessionStoreError>;

    /// Look up a session by ID.
    async fn get(&self, id: &SessionId) -> Result<Option<StoredSession>, SessionStoreError>;

    /// List sessions matching a filter. Used by runtime to scope by
    /// user / agent / lifetime without loading everything.
    ///
    /// `ctx` provides the caller's identity. The implementation
    /// should refuse to return sessions that the caller is not
    /// allowed to see (i.e. `filter.identity = None` is only safe
    /// for admin callers; non-admin must pass their own identity).
    async fn list(
        &self,
        ctx: &sylvander_protocol::SessionContext,
        filter: SessionFilter,
    ) -> Result<Vec<StoredSession>, SessionStoreError>;

    /// Full-text search over session name + user_id via SQLite FTS5.
    /// Returns matches ordered by relevance, capped at `limit`.
    ///
    /// `ctx` provides the caller's identity for scoping. Sessions
    /// not visible to `ctx.identity` are excluded.
    async fn search(
        &self,
        ctx: &sylvander_protocol::SessionContext,
        query: &str,
        limit: usize,
    ) -> Result<Vec<StoredSession>, SessionStoreError>;

    // ---- message history ----

    /// Append a message to a session's history. `seq` is auto-assigned
    /// (next integer in session). Returns the stored record (with
    /// `id` and assigned `seq`).
    ///
    /// `ctx` is what gets stored on the message — use it to
    /// attribute the message to the right identity.
    async fn append_message(
        &self,
        ctx: &sylvander_protocol::SessionContext,
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
    ///
    /// `ctx` provides the caller's identity for access control.
    /// Messages not visible to `ctx.identity` are excluded.
    async fn read_history(
        &self,
        ctx: &sylvander_protocol::SessionContext,
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

    /// Atomically retire every currently active message and append the exact
    /// replacement sequence. Used by semantic compaction so a crash can never
    /// expose a half-replaced history.
    async fn replace_active_history(
        &self,
        ctx: &sylvander_protocol::SessionContext,
        session_id: &SessionId,
        messages: Vec<ReplacementMessage>,
    ) -> Result<(), SessionStoreError>;

    /// Count non-summarized messages visible to the calling identity.
    /// Cheap O(1) on SQLite.
    async fn count_active_messages(
        &self,
        ctx: &sylvander_protocol::SessionContext,
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
