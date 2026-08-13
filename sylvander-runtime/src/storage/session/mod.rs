//! Runtime Session persistence contract and `SQLite` backend.
//!
//! The current schema owns session metadata, Agent membership, ordered
//! messages, cumulative usage, and durable turn state. A turn owns its
//! immutable effective configuration and terminal outcome, so Runtime can
//! commit an assistant reply and the corresponding completion fact in one
//! transaction. Compaction marks retired messages with `is_summarized`; they
//! remain auditable on disk while the active loop view excludes them.

mod execution_ledger;
mod model_ledger;
mod sqlite;

pub use execution_ledger::{
    RecoveryClassification, ToolExecutionPosition, ToolInvocationId, ToolRecoveryDecision,
    ToolRecoveryReason,
};
pub use model_ledger::{
    ModelExecutionPosition, ModelInvocationId, ModelRecoveryClassification, ModelRecoveryDecision,
    ModelRecoveryReason,
};
pub use sqlite::{SESSION_SCHEMA_OBJECT_NAMES, SqliteSessionStore};

use std::collections::HashMap;
use std::ops::Range;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sylvander_agent::tool::invocation::{ToolInvocationClass, ToolRecoveryPolicy};

use crate::agent_definition::{AgentId, SessionId};
use crate::session::SessionMetadata;
use sylvander_api::session::{SessionConfigOverrides, SessionEffectiveConfig};
use sylvander_api::{AgentInstanceId, CoordinationMessageId, HandoffId, TaskId, UserId};

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
    /// Monotonic revision for optimistic session configuration updates.
    #[serde(default)]
    pub config_revision: u64,
    /// Sparse session-owned values layered over Agent and channel defaults.
    #[serde(default)]
    pub config_overrides: SessionConfigOverrides,
    /// Last successfully resolved configuration. `None` is valid only while a
    /// newly constructed session awaits Runtime resolution; the current
    /// durable schema rejects unresolved persisted sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_config: Option<SessionEffectiveConfig>,
    /// Durable lifecycle state. Only archive-aware Runtime queries expose
    /// records for which this is true.
    #[serde(default)]
    pub archived: bool,
}

/// Atomic metadata-only changes that never rewrite session configuration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionMetadataPatch {
    /// Replacement display name; `None` leaves the current name unchanged.
    pub name: Option<String>,
    /// Channel-owned values merged by key into the existing metadata.
    pub external_meta: HashMap<String, JsonValue>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUsage {
    pub iterations: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `None` means one or more recorded iterations lacked pricing truth.
    pub cost_nano_usd: Option<u64>,
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
            config_revision: 0,
            config_overrides: SessionConfigOverrides::default(),
            effective_config: None,
            archived: false,
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
/// - `Assistant`: the agent's reply (may contain `tool_use` blocks)
/// - `Tool`: the result of a tool call (`parent_msg_id` points to the
///   assistant message that issued the `tool_use`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    Tool,
}

/// One persisted message.
///
/// `content` is the Runtime storage encoding for provider-neutral conversation
/// content. Runtime reconstructs an Agent `ConversationSnapshot` explicitly;
/// persisted JSON is never passed to a provider adapter as executable input.
///
/// Storage layout (`SQLite` `session_messages`):
/// - `seq` is auto-assigned (next integer in session).
/// - `id` is the `SQLite` rowid (auto-increment).
///
/// Identity / trace / priority are denormalized as real columns
/// (not stored as a JSON blob) so `SQLite` can use indexes for
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
    pub priority: Option<sylvander_api::session_context::Priority>,
    pub seq: u32,
    pub role: MessageRole,
    /// Provider-neutral conversation JSON:
    /// - user:      `{"role":"user","content":"hi"}` or `{"role":"user","content":[...]}`
    /// - assistant: `{"role":"assistant","content":[TextBlock|ToolUseBlock|...]}`
    /// - tool:      `{"role":"user","content":[{"type":"tool_result",...}]}`
    pub content: JsonValue,
    pub model_id: Option<String>,
    pub tool_name: Option<String>,
    pub parent_msg_id: Option<i64>,
    /// True once semantic compaction has folded this message into a summary.
    /// Excluded from `read_history(include_summarized=false)`.
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

/// Durable lifecycle state of a Runtime turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    Running,
    Completed,
    Failed,
    Interrupted,
}

/// Content-free classification persisted for an unsuccessful turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnFailureKind {
    UnknownSession,
    Authentication,
    AgentLoop,
    Configuration,
    Persistence,
}

/// Immutable configuration and lifecycle snapshot of one durable turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSnapshot {
    pub session_id: SessionId,
    pub turn_id: String,
    pub agent_instance_id: AgentInstanceId,
    pub config_revision: u64,
    pub effective_config: SessionEffectiveConfig,
    pub created_at: i64,
    pub state: TurnState,
    pub ended_at: Option<i64>,
    pub failure_kind: Option<TurnFailureKind>,
}

/// Inputs atomically persisted when a user turn begins.
#[derive(Debug, Clone)]
pub struct TurnStart {
    pub session_id: SessionId,
    pub turn_id: String,
    pub agent_instance_id: AgentInstanceId,
    pub config_revision: u64,
    pub effective_config: SessionEffectiveConfig,
    pub user_content: JsonValue,
    pub model_id: String,
}

/// Assistant output committed together with a successful turn terminal.
#[derive(Debug, Clone)]
pub struct TurnCompletion {
    pub session_id: SessionId,
    pub turn_id: String,
    pub assistant_content: JsonValue,
    pub model_id: String,
}

/// Complete a turn from an assistant response already durably linked to its
/// model iteration; no message is appended by this operation.
#[derive(Debug, Clone)]
pub struct PersistedTurnCompletion {
    pub invocation_id: ModelInvocationId,
    pub expected_revision: u64,
}

/// Immutable facts written before one provider request may begin.
#[derive(Debug, Clone)]
pub struct ModelIterationStart {
    pub session_id: SessionId,
    pub turn_id: String,
    pub iteration: u32,
    pub invocation_id: ModelInvocationId,
    pub model_id: String,
    pub capability_revision: String,
    pub request_digest: String,
}

/// Optimistic transition after all tool work for an iteration is durable.
#[derive(Debug, Clone)]
pub struct ModelIterationAdvance {
    pub invocation_id: ModelInvocationId,
    pub expected_revision: u64,
    pub expected_position: ModelExecutionPosition,
    pub next_position: ModelExecutionPosition,
}

/// Assistant response and its execution boundary, committed atomically.
#[derive(Debug, Clone)]
pub struct ModelResponsePersistence {
    pub invocation_id: ModelInvocationId,
    pub expected_revision: u64,
    pub assistant_content: JsonValue,
    pub model_id: String,
    pub terminal: bool,
}

/// Result of atomically persisting one provider-neutral response.
#[derive(Debug, Clone)]
pub struct ModelResponseCommit {
    pub message: StoredMessage,
    pub ledger_revision: u64,
}

/// Content-free durable model iteration used by recovery and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelIterationSnapshot {
    pub session_id: SessionId,
    pub turn_id: String,
    pub iteration: u32,
    pub invocation_id: ModelInvocationId,
    pub model_id: String,
    pub capability_revision: String,
    pub request_digest: String,
    pub position: ModelExecutionPosition,
    pub ledger_revision: u64,
    pub response_message_id: Option<i64>,
    pub response_terminal: Option<bool>,
    pub started_at: i64,
    pub updated_at: i64,
    pub recovery_decision: Option<ModelRecoveryDecision>,
    pub recovery_reason: Option<ModelRecoveryReason>,
    pub operator_action_required: bool,
    pub recovery_attempts: u32,
    pub recovery_owner: Option<String>,
    pub recovery_lease_expires_at: Option<i64>,
    pub first_interrupted_at: Option<i64>,
}

/// Atomic lease acquisition plus deterministic model recovery decision.
#[derive(Debug, Clone)]
pub struct ModelRecoveryWrite {
    pub invocation_id: ModelInvocationId,
    pub expected_revision: u64,
    pub recovery_owner: String,
    pub observed_at: i64,
    pub lease_expires_at: i64,
    pub classification: ModelRecoveryClassification,
}

/// Durable lifecycle state of one tool call inside a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallState {
    Running,
    Succeeded,
    Failed,
    Rejected,
    Abandoned,
}

/// Content-safe failure evidence supplied by the execution adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallFailureKind {
    FilesystemBoundaryPolicyViolation,
}

/// Immutable identity persisted before a tool is executed or rejected.
#[derive(Debug, Clone)]
pub struct ToolCallStart {
    pub session_id: SessionId,
    pub turn_id: String,
    pub call_id: String,
    pub invocation_id: ToolInvocationId,
    pub tool_name: String,
    pub invocation_class: Option<ToolInvocationClass>,
    pub declared_recovery_policy: ToolRecoveryPolicy,
    pub effective_recovery_policy: ToolRecoveryPolicy,
    pub capability_revision: String,
    pub input_digest: String,
}

/// Optimistic request to advance exactly one durable effect boundary.
#[derive(Debug, Clone)]
pub struct ToolCallAdvance {
    pub session_id: SessionId,
    pub turn_id: String,
    pub call_id: String,
    pub expected_revision: u64,
    pub expected_position: ToolExecutionPosition,
    pub next_position: ToolExecutionPosition,
}

/// Terminal facts committed for one previously started tool call.
#[derive(Debug, Clone)]
pub struct ToolCallCompletion {
    pub session_id: SessionId,
    pub turn_id: String,
    pub call_id: String,
    pub state: ToolCallState,
    pub failure_kind: Option<ToolCallFailureKind>,
}

/// Content-free durable tool record used for recovery and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallSnapshot {
    pub session_id: SessionId,
    pub turn_id: String,
    pub call_id: String,
    pub invocation_id: ToolInvocationId,
    pub tool_name: String,
    pub invocation_class: Option<ToolInvocationClass>,
    pub declared_recovery_policy: ToolRecoveryPolicy,
    pub effective_recovery_policy: ToolRecoveryPolicy,
    pub capability_revision: String,
    pub input_digest: String,
    pub position: ToolExecutionPosition,
    pub ledger_revision: u64,
    pub started_at: i64,
    pub updated_at: i64,
    pub state: ToolCallState,
    pub ended_at: Option<i64>,
    pub failure_kind: Option<ToolCallFailureKind>,
    pub recovery_decision: Option<ToolRecoveryDecision>,
    pub recovery_reason: Option<ToolRecoveryReason>,
    pub operator_action_required: bool,
    pub recovery_attempts: u32,
    pub recovery_owner: Option<String>,
    pub recovery_lease_expires_at: Option<i64>,
    pub first_interrupted_at: Option<i64>,
}

/// Atomic lease acquisition plus classification for one interrupted call.
#[derive(Debug, Clone)]
pub struct ToolRecoveryWrite {
    pub invocation_id: ToolInvocationId,
    pub expected_revision: u64,
    pub recovery_owner: String,
    pub observed_at: i64,
    pub lease_expires_at: i64,
    pub classification: RecoveryClassification,
}

/// Exact model-visible observation, ledger boundary, and terminal state
/// committed in one transaction.
#[derive(Debug, Clone)]
pub struct ToolResultPersistence {
    pub session_id: SessionId,
    pub turn_id: String,
    pub call_id: String,
    pub expected_revision: u64,
    pub expected_position: ToolExecutionPosition,
    pub content: JsonValue,
    pub tool_name: String,
    pub terminal_state: ToolCallState,
    pub failure_kind: Option<ToolCallFailureKind>,
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
    pub identity: Option<sylvander_api::Identity>,
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

    /// List persistent sessions for Runtime lifecycle and authorized UI paths.
    /// Boot callers pass `false`; only explicit archive discovery passes true.
    async fn list_persistent(
        &self,
        include_archived: bool,
    ) -> Result<Vec<StoredSession>, SessionStoreError>;

    /// Save or update a session record (upsert).
    async fn save(&self, session: &StoredSession) -> Result<(), SessionStoreError>;

    /// Merge mutable presentation/channel metadata without touching the
    /// configuration revision, overrides, or resolved effective config.
    async fn patch_metadata(
        &self,
        id: &SessionId,
        patch: SessionMetadataPatch,
    ) -> Result<(), SessionStoreError>;

    /// Replace sparse overrides and their resolved value when the caller's
    /// revision still matches. Returns the new monotonic revision.
    async fn update_config(
        &self,
        id: &SessionId,
        expected_revision: u64,
        overrides: SessionConfigOverrides,
        effective: SessionEffectiveConfig,
    ) -> Result<u64, SessionStoreError>;

    /// Persist the user message and immutable effective configuration in one
    /// transaction before any provider or tool work starts.
    async fn begin_turn(
        &self,
        ctx: &sylvander_api::SessionContext,
        start: TurnStart,
    ) -> Result<StoredMessage, SessionStoreError>;

    /// Read the immutable configuration and current lifecycle of one turn.
    async fn turn(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<Option<TurnSnapshot>, SessionStoreError>;

    /// Atomically append the assistant message and mark the turn completed.
    async fn complete_turn(
        &self,
        ctx: &sylvander_api::SessionContext,
        completion: TurnCompletion,
    ) -> Result<StoredMessage, SessionStoreError>;

    /// Atomically resolve a terminal model iteration and its parent turn.
    async fn complete_persisted_turn(
        &self,
        completion: PersistedTurnCompletion,
    ) -> Result<StoredMessage, SessionStoreError>;

    /// Mark a running turn unsuccessful before publishing its public terminal.
    async fn finish_turn(
        &self,
        session_id: &SessionId,
        turn_id: &str,
        state: TurnState,
        failure_kind: Option<TurnFailureKind>,
    ) -> Result<(), SessionStoreError>;

    /// Persist immutable provider-request facts before network work starts.
    async fn begin_model_iteration(
        &self,
        start: ModelIterationStart,
    ) -> Result<(), SessionStoreError>;

    /// Atomically append the response and cross its durable boundary.
    async fn persist_model_response(
        &self,
        ctx: &sylvander_api::SessionContext,
        response: ModelResponsePersistence,
    ) -> Result<ModelResponseCommit, SessionStoreError>;

    /// Advance from a persisted response after all referenced tools resolve.
    async fn advance_model_iteration(
        &self,
        advance: ModelIterationAdvance,
    ) -> Result<u64, SessionStoreError>;

    /// Read model-iteration facts in execution order for one turn.
    async fn model_iterations(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<Vec<ModelIterationSnapshot>, SessionStoreError>;

    /// Scan the latest unfinished iteration of every non-terminal turn.
    async fn interrupted_model_iterations(
        &self,
    ) -> Result<Vec<ModelIterationSnapshot>, SessionStoreError>;

    /// Acquire/renew a bounded lease and persist one deterministic decision.
    async fn classify_model_recovery(
        &self,
        write: ModelRecoveryWrite,
    ) -> Result<u64, SessionStoreError>;

    /// Persist tool identity before approval or execution can produce a
    /// terminal. The addressed turn must currently be running.
    async fn begin_tool_call(&self, start: ToolCallStart) -> Result<(), SessionStoreError>;

    /// Advance one adjacent execution boundary using a monotonic CAS.
    async fn advance_tool_call(&self, advance: ToolCallAdvance) -> Result<u64, SessionStoreError>;

    /// Atomically append the model-visible observation and mark it durable.
    async fn persist_tool_result(
        &self,
        ctx: &sylvander_api::SessionContext,
        result: ToolResultPersistence,
    ) -> Result<u64, SessionStoreError>;

    /// Atomically replace a running tool call with exactly one terminal.
    async fn finish_tool_call(
        &self,
        completion: ToolCallCompletion,
    ) -> Result<(), SessionStoreError>;

    /// Read durable tool lifecycle facts in start order for one turn.
    async fn tool_calls(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<Vec<ToolCallSnapshot>, SessionStoreError>;

    /// Scan running calls owned by turns that were non-terminal at boot.
    async fn interrupted_tool_calls(&self) -> Result<Vec<ToolCallSnapshot>, SessionStoreError>;

    /// Acquire/renew a bounded lease and persist one deterministic decision.
    async fn classify_tool_recovery(
        &self,
        write: ToolRecoveryWrite,
    ) -> Result<u64, SessionStoreError>;

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
        cost_nano_usd: Option<u64>,
    ) -> Result<SessionUsage, SessionStoreError>;

    async fn usage(&self, id: &SessionId) -> Result<SessionUsage, SessionStoreError>;

    /// Hard-delete. Cascades through `session_agents` and
    /// `session_messages`. Use only on explicit user action.
    async fn delete(&self, id: &SessionId) -> Result<(), SessionStoreError>;

    /// Look up a session by ID.
    async fn get(&self, id: &SessionId) -> Result<Option<StoredSession>, SessionStoreError>;

    /// Look up a session by ID without hiding an archived record.
    ///
    /// This is a Runtime-internal authorization primitive for restore flows;
    /// transports must never receive the storage handle or raw record.
    async fn get_including_archived(
        &self,
        id: &SessionId,
    ) -> Result<Option<StoredSession>, SessionStoreError>;

    /// List sessions matching a filter. Used by runtime to scope by
    /// user / agent / lifetime without loading everything.
    ///
    /// `ctx` provides the caller's identity. The implementation
    /// should refuse to return sessions that the caller is not
    /// allowed to see (i.e. `filter.identity = None` is only safe
    /// for admin callers; non-admin must pass their own identity).
    async fn list(
        &self,
        ctx: &sylvander_api::SessionContext,
        filter: SessionFilter,
    ) -> Result<Vec<StoredSession>, SessionStoreError>;

    /// Full-text search over session name + `user_id` via `SQLite` FTS5.
    /// Returns matches ordered by relevance, capped at `limit`.
    ///
    /// `ctx` provides the caller's identity for scoping. Sessions
    /// not visible to `ctx.identity` are excluded.
    async fn search(
        &self,
        ctx: &sylvander_api::SessionContext,
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
    #[allow(clippy::too_many_arguments)]
    async fn append_message(
        &self,
        ctx: &sylvander_api::SessionContext,
        session_id: &SessionId,
        role: MessageRole,
        content: JsonValue,
        model_id: Option<&str>,
        tool_name: Option<&str>,
        parent_msg_id: Option<i64>,
    ) -> Result<StoredMessage, SessionStoreError>;

    /// Read all messages for a session, ordered by `seq` ascending.
    /// `include_summarized=false` skips compacted messages.
    /// `limit` caps the result (most recent N if Some).
    ///
    /// `ctx` provides the caller's identity for access control.
    /// Messages not visible to `ctx.identity` are excluded.
    async fn read_history(
        &self,
        ctx: &sylvander_api::SessionContext,
        session_id: &SessionId,
        include_summarized: bool,
        limit: Option<usize>,
    ) -> Result<Vec<StoredMessage>, SessionStoreError>;

    /// Mark a contiguous range of messages as summarized.
    /// Called when semantic compaction produces a summary that supersedes
    /// older messages.
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
        ctx: &sylvander_api::SessionContext,
        session_id: &SessionId,
        messages: Vec<ReplacementMessage>,
    ) -> Result<(), SessionStoreError>;

    /// Count non-summarized messages visible to the calling identity.
    /// Cheap O(1) on `SQLite`.
    async fn count_active_messages(
        &self,
        ctx: &sylvander_api::SessionContext,
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
    #[error("session store schema is not the exact current schema")]
    IncompatibleSchema,
    #[error("session store integrity check failed")]
    CorruptSchema,
    #[error("session not found: {0}")]
    NotFound(SessionId),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("session configuration revision conflict: expected {expected}, actual {actual}")]
    ConfigConflict { expected: u64, actual: u64 },
    #[error("session membership revision conflict: expected {expected:?}, actual {actual:?}")]
    MembershipConflict {
        expected: Option<u64>,
        actual: Option<u64>,
    },
    #[error("session topology revision conflict: expected {expected:?}, actual {actual:?}")]
    TopologyConflict {
        expected: Option<u64>,
        actual: Option<u64>,
    },
    #[error("task {task_id} revision conflict: expected {expected:?}, actual {actual:?}")]
    TaskConflict {
        task_id: TaskId,
        expected: Option<u64>,
        actual: Option<u64>,
    },
    #[error("handoff {handoff_id:?} revision conflict: expected {expected:?}, actual {actual:?}")]
    HandoffConflict {
        handoff_id: HandoffId,
        expected: Option<u64>,
        actual: Option<u64>,
    },
    #[error("message {message_id:?} revision conflict: expected {expected:?}, actual {actual:?}")]
    MessageConflict {
        message_id: CoordinationMessageId,
        expected: Option<u64>,
        actual: Option<u64>,
    },
}

impl From<rusqlite::Error> for SessionStoreError {
    fn from(e: rusqlite::Error) -> Self {
        SessionStoreError::Store(e.to_string())
    }
}
