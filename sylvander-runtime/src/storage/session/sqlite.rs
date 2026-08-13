//! Runtime-owned SQLite-backed [`SessionStore`].
//!
//! One store owns one `rusqlite::Connection`, serialized by
//! `tokio::sync::Mutex`. Every database operation runs through
//! `spawn_blocking`, so `SQLite` work never occupies an async executor thread.
//! Runtime owns the store process-wide and applies its own bounded admission.
//!
//! A completely empty database is initialized directly at the current schema.
//! Existing databases must match the application id, schema version, and
//! complete `SQLite` object set exactly; Sylvander does not repair or migrate an
//! older, newer, unmanaged, or damaged session database.
//!
//! Current transcript encoding stores provider-neutral conversation JSON.
//! Runtime validates and reconstructs the Agent conversation when restoring a
//! Session; storage bytes are not a provider wire request.

use std::collections::HashSet;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use sylvander_agent::tool::invocation::{ToolInvocationClass, ToolRecoveryPolicy};
use sylvander_api::AgentInstanceId;
use sylvander_api::session_context::Priority;
use tokio::sync::Mutex;
use tokio::task;

use crate::agent_definition::{AgentId, SessionId};
use crate::session::SessionMetadata;

use super::{
    MessageRole, ModelExecutionPosition, ModelInvocationId, ModelIterationAdvance,
    ModelIterationSnapshot, ModelIterationStart, ModelRecoveryDecision, ModelRecoveryReason,
    ModelRecoveryWrite, ModelResponseCommit, ModelResponsePersistence, PersistedTurnCompletion,
    ReplacementMessage, SessionFilter, SessionLifetime, SessionMetadataPatch, SessionStore,
    SessionStoreError, SessionUsage, StoredMessage, StoredSession, ToolCallAdvance,
    ToolCallCompletion, ToolCallFailureKind, ToolCallSnapshot, ToolCallStart, ToolCallState,
    ToolExecutionPosition, ToolInvocationId, ToolRecoveryDecision, ToolRecoveryReason,
    ToolRecoveryWrite, ToolResultPersistence, TurnCompletion, TurnFailureKind, TurnSnapshot,
    TurnStart, TurnState,
};

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SQLITE_SYNCHRONOUS_FULL: i64 = 2;

/// SQLite-backed session store.
#[derive(Clone)]
pub struct SqliteSessionStore {
    inner: Arc<StoreInner>,
}

struct StoreInner {
    /// Synchronous `SQLite` connection. Guarded by `Mutex` so async tasks
    /// serialize their `spawn_blocking` calls into a single thread.
    conn: Mutex<Connection>,
    /// Exact non-Session objects admitted when this connection was opened.
    /// Retaining the allowlist lets health checks revalidate the live schema
    /// without weakening shared-database ownership rules.
    allowed_foreign_objects: Vec<String>,
}

impl std::fmt::Debug for SqliteSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSessionStore").finish_non_exhaustive()
    }
}

impl SqliteSessionStore {
    /// Open or create a database at `path`.
    ///
    /// An empty file is initialized at the current schema. Every non-empty
    /// file must already be an exact current Sylvander session database.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, SessionStoreError> {
        Self::open_with_allowed_foreign_objects(path, Vec::new()).await
    }

    /// Open the session store in a database shared with another component.
    ///
    /// `allowed_foreign_objects` must be the other component's complete
    /// current owned-object allowlist. Session objects remain exact-match
    /// validated; foreign objects are namespace-checked here and validated by
    /// their owning component.
    pub async fn open_shared(
        path: impl AsRef<Path>,
        allowed_foreign_objects: &[&str],
    ) -> Result<Self, SessionStoreError> {
        Self::open_with_allowed_foreign_objects(
            path,
            allowed_foreign_objects
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
        )
        .await
    }

    async fn open_with_allowed_foreign_objects(
        path: impl AsRef<Path>,
        allowed_foreign_objects: Vec<String>,
    ) -> Result<Self, SessionStoreError> {
        let path = path.as_ref().to_path_buf();
        task::spawn_blocking(move || -> Result<Self, SessionStoreError> {
            let conn = Connection::open(&path).map_err(sqlite_err)?;
            configure_durable_connection(&conn)?;
            Self::init_schema_with_foreign_objects(&conn, &allowed_foreign_objects)?;
            Ok(Self {
                inner: Arc::new(StoreInner {
                    conn: Mutex::new(conn),
                    allowed_foreign_objects,
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
            conn.execute_batch("PRAGMA foreign_keys=ON;")
                .map_err(sqlite_err)?;
            Self::init_schema(&conn)?;
            Ok(Self {
                inner: Arc::new(StoreInner {
                    conn: Mutex::new(conn),
                    allowed_foreign_objects: Vec::new(),
                }),
            })
        })
        .await
        .map_err(|e| SessionStoreError::Store(format!("blocking task panicked: {e}")))?
    }

    /// Initialize an empty database or validate an exact current database.
    fn init_schema(conn: &Connection) -> Result<(), SessionStoreError> {
        Self::init_schema_with_foreign_objects(conn, &[])
    }

    fn init_schema_with_foreign_objects(
        conn: &Connection,
        allowed_foreign_objects: &[String],
    ) -> Result<(), SessionStoreError> {
        validate_allowed_foreign_objects(allowed_foreign_objects)?;
        let version = conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_err)?;
        let application_id = conn
            .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_err)?;
        let object_count = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sqlite_err)?;
        match (version, application_id, object_count) {
            (0, 0, 0) => conn.execute_batch(SCHEMA_SQL).map_err(sqlite_err)?,
            (SESSION_SCHEMA_VERSION, SESSION_APPLICATION_ID, _) => {
                validate_schema(conn, allowed_foreign_objects)?;
            }
            _ => return Err(SessionStoreError::IncompatibleSchema),
        }
        let integrity = conn
            .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .map_err(sqlite_err)?;
        if integrity == "ok" {
            validate_owned_foreign_keys(conn)
        } else {
            Err(SessionStoreError::CorruptSchema)
        }
    }

    /// Acquire the lock and run a closure against the connection on
    /// a blocking thread. Centralizes the `spawn_blocking` boilerplate.
    ///
    /// The closure returns `Result<T, SessionStoreError>` directly
    /// (not `rusqlite::Result`) so it can return our own error type
    /// for things like `NotFound` without a lossy conversion.
    pub(crate) async fn run<F, T>(&self, f: F) -> Result<T, SessionStoreError>
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

    /// Revalidate the live database without exposing its path or contents.
    ///
    /// Runtime health uses the result as a boolean signal. The concrete error
    /// remains inside the storage boundary because schema and filesystem
    /// details are not part of the public operational contract.
    pub(crate) async fn verify_health(&self) -> Result<(), SessionStoreError> {
        let allowed = self.inner.allowed_foreign_objects.clone();
        self.run(move |connection| {
            let version = connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .map_err(sqlite_err)?;
            let application_id = connection
                .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
                .map_err(sqlite_err)?;
            if version != SESSION_SCHEMA_VERSION || application_id != SESSION_APPLICATION_ID {
                return Err(SessionStoreError::IncompatibleSchema);
            }
            validate_schema(connection, &allowed)?;
            let quick_check = connection
                .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                .map_err(sqlite_err)?;
            if quick_check != "ok" {
                return Err(SessionStoreError::CorruptSchema);
            }
            validate_owned_foreign_keys(connection)
        })
        .await
    }
}

fn configure_durable_connection(conn: &Connection) -> Result<(), SessionStoreError> {
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT).map_err(sqlite_err)?;
    let journal_mode = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
        .map_err(sqlite_err)?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(SessionStoreError::Store(
            "session database cannot enable durable journal mode".into(),
        ));
    }
    conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
        .map_err(sqlite_err)?;
    let synchronous = conn
        .query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
        .map_err(sqlite_err)?;
    let foreign_keys = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
        .map_err(sqlite_err)?;
    if synchronous != SQLITE_SYNCHRONOUS_FULL || foreign_keys != 1 {
        return Err(SessionStoreError::Store(
            "session database durability controls are unavailable".into(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const SESSION_SCHEMA_VERSION: i64 = 13;
const SESSION_APPLICATION_ID: i64 = 0x5359_5353;

/// `SQLite` objects owned and exact-match validated by the session store.
///
/// A shared database caller must pass this complete list to every other
/// component that is allowed to coexist with the session store.
pub const SESSION_SCHEMA_OBJECT_NAMES: &[&str] = &[
    "sessions",
    "session_agents",
    "session_agent_instances",
    "session_governance",
    "session_topology",
    "agent_relations",
    "coordination_tasks",
    "task_dependencies",
    "task_handoffs",
    "coordination_messages",
    "coordination_waits",
    "coordination_progress",
    "governance_cases",
    "moderator_decisions",
    "agent_workspace_views",
    "workspace_integrations",
    "session_messages",
    "agent_history_fork_receipts",
    "session_usage",
    "session_turns",
    "session_turn_iterations",
    "session_tool_calls",
    "idx_messages_user",
    "idx_messages_agent",
    "idx_messages_agent_instance",
    "idx_messages_trace",
    "idx_sessions_lifetime",
    "idx_sessions_user",
    "idx_sessions_updated",
    "idx_tasks_assignee_state",
    "idx_handoffs_arbitrator_state",
    "idx_messages_recipient_state",
    "idx_coordination_waits_current",
    "idx_coordination_progress_task",
    "idx_governance_cases_moderator_state",
    "idx_one_active_agent_workspace",
    "idx_one_active_workspace_integration",
    "idx_session_agents_agent",
    "idx_agent_instances_definition",
    "idx_agent_instances_state",
    "idx_one_session_moderator",
    "idx_messages_session",
    "idx_messages_unsummarized",
    "idx_tool_calls_turn",
    "idx_tool_calls_recovery",
    "idx_turn_iterations_recovery",
    "idx_running_turn_per_agent_instance",
];

const SCHEMA_SQL: &str = r"
BEGIN IMMEDIATE;
PRAGMA application_id=1398362963;
-- Session metadata
CREATE TABLE sessions (
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
CREATE TABLE session_agents (
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_id        TEXT NOT NULL,
    joined_at       INTEGER NOT NULL,
    PRIMARY KEY (session_id, agent_id)
);

-- First-class running Agent participants. `agent_id` identifies the reusable
-- definition while `instance_id` identifies one concrete Session actor.
CREATE TABLE session_agent_instances (
    instance_id        TEXT PRIMARY KEY,
    session_id         TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    membership_ordinal INTEGER NOT NULL CHECK(membership_ordinal >= 0),
    agent_id           TEXT NOT NULL,
    definition_revision INTEGER NOT NULL CHECK(definition_revision > 0),
    origin_json        TEXT NOT NULL,
    role               TEXT NOT NULL CHECK(role IN ('moderator','coordinator','worker','reviewer','specialist','observer')),
    role_swarm_id      TEXT,
    history_view_json  TEXT NOT NULL,
    approval_route_json TEXT NOT NULL,
    state              TEXT NOT NULL CHECK(state IN ('created','ready','running','waiting_message','waiting_approval','completed','failed','cancelled','manual_reconciliation')),
    capability_revision TEXT NOT NULL CHECK(length(trim(capability_revision)) > 0),
    lifecycle_revision INTEGER NOT NULL DEFAULT 0 CHECK(lifecycle_revision >= 0),
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL,
    CHECK((role = 'coordinator' AND role_swarm_id IS NOT NULL)
       OR (role != 'coordinator' AND role_swarm_id IS NULL)),
    UNIQUE(session_id, instance_id),
    UNIQUE(session_id, instance_id, role),
    UNIQUE(session_id, membership_ordinal)
);

-- Exactly one current root moderator signs final Session arbitration. The
-- fixed role column makes the foreign key prove that the referenced instance
-- really is the moderator rather than merely a participant.
CREATE TABLE session_governance (
    session_id          TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    moderator_instance_id TEXT NOT NULL,
    moderator_role      TEXT NOT NULL DEFAULT 'moderator' CHECK(moderator_role = 'moderator'),
    governance_revision TEXT NOT NULL CHECK(length(trim(governance_revision)) > 0),
    membership_revision INTEGER NOT NULL CHECK(membership_revision >= 0),
    lease_epoch         INTEGER NOT NULL CHECK(lease_epoch > 0),
    fencing_token       INTEGER NOT NULL CHECK(fencing_token > 0),
    updated_at          INTEGER NOT NULL,
    FOREIGN KEY(session_id, moderator_instance_id, moderator_role)
        REFERENCES session_agent_instances(session_id, instance_id, role)
        ON DELETE CASCADE
);

CREATE TABLE session_topology (
    session_id          TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    membership_revision INTEGER NOT NULL CHECK(membership_revision >= 0),
    topology_revision   INTEGER NOT NULL CHECK(topology_revision >= 0),
    updated_at          INTEGER NOT NULL
);

CREATE TABLE agent_relations (
    session_id          TEXT NOT NULL,
    relation_ordinal    INTEGER NOT NULL CHECK(relation_ordinal >= 0),
    source_instance_id  TEXT NOT NULL,
    target_instance_id  TEXT NOT NULL,
    relation_kind       TEXT NOT NULL CHECK(relation_kind IN ('parent_of','peer','reviews')),
    created_at          INTEGER NOT NULL,
    PRIMARY KEY(session_id, relation_ordinal),
    FOREIGN KEY(session_id) REFERENCES session_topology(session_id) ON DELETE CASCADE,
    FOREIGN KEY(session_id, source_instance_id)
        REFERENCES session_agent_instances(session_id, instance_id) ON DELETE CASCADE,
    FOREIGN KEY(session_id, target_instance_id)
        REFERENCES session_agent_instances(session_id, instance_id) ON DELETE CASCADE
);

CREATE TABLE coordination_tasks (
    task_id             TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    membership_revision INTEGER NOT NULL CHECK(membership_revision >= 0),
    parent_task_id      TEXT REFERENCES coordination_tasks(task_id),
    created_by_instance_id TEXT NOT NULL,
    assigned_to_instance_id TEXT,
    objective           TEXT NOT NULL CHECK(length(trim(objective)) > 0),
    state               TEXT NOT NULL CHECK(state IN ('proposed','ready','running','blocked','awaiting_review','completed','failed','cancelled')),
    token_budget        INTEGER NOT NULL CHECK(token_budget > 0),
    consumed_tokens     INTEGER NOT NULL CHECK(consumed_tokens >= 0 AND consumed_tokens <= token_budget),
    max_handoffs        INTEGER NOT NULL CHECK(max_handoffs >= 0),
    handoff_count       INTEGER NOT NULL CHECK(handoff_count >= 0 AND handoff_count <= max_handoffs),
    revision            INTEGER NOT NULL CHECK(revision >= 0),
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);

CREATE TABLE task_dependencies (
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    prerequisite_task_id TEXT NOT NULL REFERENCES coordination_tasks(task_id) ON DELETE CASCADE,
    dependent_task_id   TEXT NOT NULL REFERENCES coordination_tasks(task_id) ON DELETE CASCADE,
    created_at          INTEGER NOT NULL,
    PRIMARY KEY(session_id, prerequisite_task_id, dependent_task_id),
    CHECK(prerequisite_task_id != dependent_task_id)
);

CREATE TABLE task_handoffs (
    handoff_id          TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    task_id             TEXT NOT NULL REFERENCES coordination_tasks(task_id),
    from_instance_id    TEXT NOT NULL,
    to_instance_id      TEXT NOT NULL,
    requested_by_instance_id TEXT NOT NULL,
    arbitrator_instance_id TEXT NOT NULL,
    task_revision       INTEGER NOT NULL CHECK(task_revision >= 0),
    topology_revision   INTEGER NOT NULL CHECK(topology_revision >= 0),
    reason              TEXT NOT NULL CHECK(length(trim(reason)) > 0),
    state               TEXT NOT NULL CHECK(state IN ('proposed','awaiting_arbitration','accepted','rejected','expired','cancelled')),
    revision            INTEGER NOT NULL CHECK(revision >= 0),
    expires_at          INTEGER NOT NULL,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    CHECK(from_instance_id != to_instance_id)
);

CREATE TABLE coordination_messages (
    message_id          TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    sender_instance_id  TEXT NOT NULL,
    recipient_instance_id TEXT NOT NULL,
    task_id             TEXT REFERENCES coordination_tasks(task_id),
    message_kind        TEXT NOT NULL CHECK(message_kind IN ('task','progress','evidence','question','decision','control')),
    payload             TEXT NOT NULL CHECK(length(trim(payload)) > 0),
    topology_revision   INTEGER NOT NULL CHECK(topology_revision >= 0),
    route_json          TEXT NOT NULL,
    max_hops            INTEGER NOT NULL CHECK(max_hops > 0),
    state               TEXT NOT NULL CHECK(state IN ('pending','claimed','delivered','acknowledged','expired','dead_letter')),
    delivery_attempts   INTEGER NOT NULL CHECK(delivery_attempts >= 0),
    lease_owner_instance_id TEXT,
    lease_epoch         INTEGER NOT NULL DEFAULT 0 CHECK(lease_epoch >= 0),
    lease_expires_at    INTEGER,
    revision            INTEGER NOT NULL CHECK(revision >= 0),
    expires_at          INTEGER NOT NULL,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    CHECK((state = 'claimed' AND lease_owner_instance_id IS NOT NULL AND lease_expires_at IS NOT NULL)
       OR (state != 'claimed' AND lease_owner_instance_id IS NULL AND lease_expires_at IS NULL))
);

CREATE INDEX idx_tasks_assignee_state
    ON coordination_tasks(session_id, assigned_to_instance_id, state);
CREATE INDEX idx_handoffs_arbitrator_state
    ON task_handoffs(session_id, arbitrator_instance_id, state);
CREATE INDEX idx_messages_recipient_state
    ON coordination_messages(session_id, recipient_instance_id, state, created_at);

-- Current wait-for graph. Revision fences prevent stale edges from surviving
-- task progress or topology replacement after a crash.
CREATE TABLE coordination_waits (
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    task_id             TEXT NOT NULL REFERENCES coordination_tasks(task_id) ON DELETE CASCADE,
    waiter_instance_id  TEXT NOT NULL,
    awaited_instance_id TEXT NOT NULL,
    task_revision       INTEGER NOT NULL CHECK(task_revision >= 0),
    topology_revision   INTEGER NOT NULL CHECK(topology_revision >= 0),
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    PRIMARY KEY(session_id, task_id, waiter_instance_id, awaited_instance_id),
    FOREIGN KEY(session_id, waiter_instance_id)
        REFERENCES session_agent_instances(session_id, instance_id) ON DELETE CASCADE,
    FOREIGN KEY(session_id, awaited_instance_id)
        REFERENCES session_agent_instances(session_id, instance_id) ON DELETE CASCADE,
    CHECK(waiter_instance_id != awaited_instance_id)
);

CREATE INDEX idx_coordination_waits_current
    ON coordination_waits(session_id, task_id, task_revision, topology_revision);

-- Append-only evidence of useful progress. The stable observation id makes
-- producer retry idempotent across process failure.
CREATE TABLE coordination_progress (
    observation_id      TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    task_id             TEXT NOT NULL REFERENCES coordination_tasks(task_id) ON DELETE CASCADE,
    agent_instance_id   TEXT NOT NULL,
    task_revision       INTEGER NOT NULL CHECK(task_revision >= 0),
    consumed_tokens     INTEGER NOT NULL CHECK(consumed_tokens >= 0),
    evidence_digest     TEXT,
    observed_at         INTEGER NOT NULL,
    FOREIGN KEY(session_id, agent_instance_id)
        REFERENCES session_agent_instances(session_id, instance_id) ON DELETE CASCADE
);

CREATE INDEX idx_coordination_progress_task
    ON coordination_progress(session_id, task_id, observed_at, observation_id);

CREATE TABLE governance_cases (
    case_id             TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    moderator_instance_id TEXT NOT NULL,
    membership_revision INTEGER NOT NULL CHECK(membership_revision >= 0),
    topology_revision   INTEGER NOT NULL CHECK(topology_revision >= 0),
    moderator_lease_epoch INTEGER NOT NULL CHECK(moderator_lease_epoch > 0),
    moderator_fencing_token INTEGER NOT NULL CHECK(moderator_fencing_token > 0),
    findings_json       TEXT NOT NULL,
    state               TEXT NOT NULL CHECK(state IN ('open','decided','applying','applied','expired')),
    revision            INTEGER NOT NULL CHECK(revision >= 0),
    expires_at          INTEGER NOT NULL,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    FOREIGN KEY(session_id, moderator_instance_id)
        REFERENCES session_agent_instances(session_id, instance_id)
);

CREATE TABLE moderator_decisions (
    case_id             TEXT PRIMARY KEY REFERENCES governance_cases(case_id) ON DELETE CASCADE,
    decided_by_instance_id TEXT NOT NULL,
    moderator_lease_epoch INTEGER NOT NULL CHECK(moderator_lease_epoch > 0),
    moderator_fencing_token INTEGER NOT NULL CHECK(moderator_fencing_token > 0),
    verdict_json        TEXT NOT NULL,
    rationale           TEXT NOT NULL CHECK(length(trim(rationale)) > 0),
    evidence_refs_json  TEXT NOT NULL,
    decided_at          INTEGER NOT NULL
);

CREATE INDEX idx_governance_cases_moderator_state
    ON governance_cases(session_id, moderator_instance_id, state, created_at);

CREATE TABLE agent_workspace_views (
    view_id             TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_instance_id   TEXT NOT NULL,
    membership_revision INTEGER NOT NULL CHECK(membership_revision >= 0),
    access_kind         TEXT NOT NULL CHECK(access_kind IN ('read_only','read_write')),
    isolation_kind      TEXT NOT NULL CHECK(isolation_kind IN ('shared','isolated_worktree')),
    source_workspace    TEXT NOT NULL,
    effective_workspace TEXT NOT NULL,
    target_id           TEXT,
    branch              TEXT,
    base_revision       TEXT,
    state               TEXT NOT NULL CHECK(state IN ('provisioning','active','integrating','integrated','conflicted','released','manual_reconciliation')),
    lease_epoch         INTEGER NOT NULL CHECK(lease_epoch > 0),
    fencing_token       INTEGER NOT NULL CHECK(fencing_token > 0),
    revision            INTEGER NOT NULL CHECK(revision >= 0),
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    FOREIGN KEY(session_id, agent_instance_id)
        REFERENCES session_agent_instances(session_id, instance_id)
);

CREATE UNIQUE INDEX idx_one_active_agent_workspace
    ON agent_workspace_views(session_id, agent_instance_id)
    WHERE state IN ('provisioning','active','integrating','conflicted','manual_reconciliation');

CREATE TABLE workspace_integrations (
    integration_id      TEXT PRIMARY KEY,
    view_id             TEXT NOT NULL REFERENCES agent_workspace_views(view_id) ON DELETE CASCADE,
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_instance_id   TEXT NOT NULL,
    approved_by_instance_id TEXT NOT NULL,
    membership_revision INTEGER NOT NULL CHECK(membership_revision >= 0),
    topology_revision   INTEGER NOT NULL CHECK(topology_revision >= 0),
    view_revision       INTEGER NOT NULL CHECK(view_revision >= 0),
    lease_epoch         INTEGER NOT NULL CHECK(lease_epoch > 0),
    fencing_token       INTEGER NOT NULL CHECK(fencing_token > 0),
    review_digest       TEXT NOT NULL CHECK(length(trim(review_digest)) > 0),
    target_revision     TEXT NOT NULL CHECK(length(trim(target_revision)) > 0),
    approved_at         INTEGER NOT NULL,
    state               TEXT NOT NULL CHECK(state IN ('approved','applying','applied','conflicted','manual_reconciliation')),
    revision            INTEGER NOT NULL CHECK(revision >= 0),
    updated_at          INTEGER NOT NULL
);

CREATE UNIQUE INDEX idx_one_active_workspace_integration
    ON workspace_integrations(view_id)
    WHERE state IN ('approved','applying','manual_reconciliation');

-- Messages (one row per user/assistant/tool message)
--
-- Identity / trace / priority are denormalized into real columns
-- (not a JSON blob) so SQLite can use indexes for per-user / per-
-- trace lookups. Adding a new SessionContext field means
-- `ALTER TABLE ADD COLUMN`, not editing a json blob.
CREATE TABLE session_messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    role            TEXT NOT NULL,
    content_json    TEXT NOT NULL,
    -- Denormalized identity (copied from SessionContext at write time).
    user_id         TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    agent_instance_id TEXT,
    -- Denormalized request metadata (same — copied at write time).
    trace_id        TEXT,
    priority        TEXT,
    model_id        TEXT,
    tool_name       TEXT,
    parent_msg_id   INTEGER REFERENCES session_messages(id) ON DELETE SET NULL,
    is_summarized   INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    UNIQUE(session_id, seq),
    FOREIGN KEY(session_id, agent_instance_id)
        REFERENCES session_agent_instances(session_id, instance_id)
        ON DELETE CASCADE
);

-- Atomic receipt proving that a fork history prefix, including an empty one,
-- has been fully materialized. The child id is the replay/idempotency key.
CREATE TABLE agent_history_fork_receipts (
    child_instance_id  TEXT PRIMARY KEY,
    session_id         TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    parent_instance_id TEXT NOT NULL,
    base_sequence      INTEGER NOT NULL CHECK(base_sequence >= 0),
    copied_messages    INTEGER NOT NULL CHECK(copied_messages >= 0),
    materialized_at    INTEGER NOT NULL,
    FOREIGN KEY(session_id, parent_instance_id)
        REFERENCES session_agent_instances(session_id, instance_id) ON DELETE CASCADE,
    FOREIGN KEY(session_id, child_instance_id)
        REFERENCES session_agent_instances(session_id, instance_id) ON DELETE CASCADE,
    CHECK(parent_instance_id != child_instance_id)
);

CREATE TABLE session_usage (
    session_id      TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    iterations      INTEGER NOT NULL DEFAULT 0,
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    cost_nano_usd   INTEGER NOT NULL DEFAULT 0,
    cost_complete   INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE session_turns (
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_id         TEXT NOT NULL,
    agent_instance_id TEXT NOT NULL,
    config_revision INTEGER NOT NULL,
    effective_config TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    state           TEXT NOT NULL CHECK(state IN ('running','completed','failed','interrupted')),
    ended_at        INTEGER,
    failure_kind    TEXT,
    CHECK((state = 'running' AND ended_at IS NULL) OR (state != 'running' AND ended_at IS NOT NULL)),
    CHECK(failure_kind IS NULL OR state = 'failed'),
    PRIMARY KEY (session_id, turn_id)
);

CREATE TABLE session_turn_iterations (
    session_id      TEXT NOT NULL,
    turn_id         TEXT NOT NULL,
    iteration       INTEGER NOT NULL CHECK(iteration > 0),
    invocation_id   TEXT NOT NULL UNIQUE,
    model_id        TEXT NOT NULL,
    capability_revision TEXT NOT NULL,
    request_digest  TEXT NOT NULL,
    position        TEXT NOT NULL CHECK(position IN ('model_started','response_persisted','tools_resolved')),
    ledger_revision INTEGER NOT NULL DEFAULT 0,
    response_message_id INTEGER REFERENCES session_messages(id) ON DELETE RESTRICT,
    response_terminal INTEGER CHECK(response_terminal IN (0, 1)),
    recovery_decision TEXT,
    recovery_reason TEXT,
    operator_action_required INTEGER NOT NULL DEFAULT 0,
    recovery_attempts INTEGER NOT NULL DEFAULT 0,
    recovery_owner TEXT,
    recovery_lease_expires_at INTEGER,
    first_interrupted_at INTEGER,
    started_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    CHECK((position = 'model_started' AND response_message_id IS NULL AND response_terminal IS NULL)
       OR (position != 'model_started' AND response_message_id IS NOT NULL AND response_terminal IS NOT NULL)),
    PRIMARY KEY (session_id, turn_id, iteration),
    FOREIGN KEY (session_id, turn_id)
        REFERENCES session_turns(session_id, turn_id) ON DELETE CASCADE
);

CREATE TABLE session_tool_calls (
    session_id      TEXT NOT NULL,
    turn_id         TEXT NOT NULL,
    call_id         TEXT NOT NULL,
    invocation_id   TEXT NOT NULL UNIQUE,
    tool_name       TEXT NOT NULL,
    invocation_class TEXT,
    declared_recovery_policy TEXT NOT NULL,
    effective_recovery_policy TEXT NOT NULL,
    capability_revision TEXT NOT NULL,
    input_digest    TEXT NOT NULL,
    position        TEXT NOT NULL CHECK(position IN ('prepared','authorized','effect_started','effect_committed','result_persisted')),
    ledger_revision INTEGER NOT NULL DEFAULT 0,
    started_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    state           TEXT NOT NULL CHECK(state IN ('running','succeeded','failed','rejected','abandoned')),
    ended_at        INTEGER,
    failure_kind    TEXT,
    recovery_decision TEXT,
    recovery_reason TEXT,
    operator_action_required INTEGER NOT NULL DEFAULT 0,
    recovery_attempts INTEGER NOT NULL DEFAULT 0,
    recovery_owner TEXT,
    recovery_lease_expires_at INTEGER,
    first_interrupted_at INTEGER,
    CHECK((state = 'running' AND ended_at IS NULL) OR (state != 'running' AND ended_at IS NOT NULL)),
    CHECK(failure_kind IS NULL OR state = 'failed'),
    PRIMARY KEY (session_id, turn_id, call_id),
    FOREIGN KEY (session_id, turn_id)
        REFERENCES session_turns(session_id, turn_id) ON DELETE CASCADE
);

CREATE INDEX idx_messages_user
    ON session_messages(user_id, session_id);
CREATE INDEX idx_messages_agent
    ON session_messages(agent_id);
CREATE INDEX idx_messages_agent_instance
    ON session_messages(session_id, agent_instance_id, seq);
CREATE INDEX idx_messages_trace
    ON session_messages(trace_id) WHERE trace_id IS NOT NULL;

-- Boot filter: persistent + non-archived
CREATE INDEX idx_sessions_lifetime
    ON sessions(lifetime, is_archived);
CREATE INDEX idx_sessions_user
    ON sessions(user_id, created_at DESC);
CREATE INDEX idx_sessions_updated
    ON sessions(updated_at DESC);
CREATE INDEX idx_session_agents_agent
    ON session_agents(agent_id);
CREATE INDEX idx_agent_instances_definition
    ON session_agent_instances(agent_id, definition_revision);
CREATE INDEX idx_agent_instances_state
    ON session_agent_instances(session_id, state, updated_at, instance_id);
CREATE UNIQUE INDEX idx_one_session_moderator
    ON session_agent_instances(session_id) WHERE role = 'moderator';
CREATE INDEX idx_messages_session
    ON session_messages(session_id, seq);
CREATE INDEX idx_messages_unsummarized
    ON session_messages(session_id, is_summarized);
CREATE INDEX idx_tool_calls_turn
    ON session_tool_calls(session_id, turn_id, started_at, call_id);
CREATE INDEX idx_tool_calls_recovery
    ON session_tool_calls(state, position, updated_at, invocation_id);
CREATE INDEX idx_turn_iterations_recovery
    ON session_turn_iterations(position, updated_at, invocation_id);
CREATE UNIQUE INDEX idx_running_turn_per_agent_instance
    ON session_turns(session_id, agent_instance_id) WHERE state = 'running';
PRAGMA user_version=13;
COMMIT;
";

fn validate_schema(
    conn: &Connection,
    allowed_foreign_objects: &[String],
) -> Result<(), SessionStoreError> {
    let expected = Connection::open_in_memory().map_err(sqlite_err)?;
    expected.execute_batch(SCHEMA_SQL).map_err(sqlite_err)?;
    let actual = schema_objects(conn)?;
    validate_object_namespace(&actual, allowed_foreign_objects)?;
    let actual_owned = owned_schema_objects(actual);
    if actual_owned == schema_objects(&expected)? {
        Ok(())
    } else {
        Err(SessionStoreError::IncompatibleSchema)
    }
}

fn validate_allowed_foreign_objects(
    allowed_foreign_objects: &[String],
) -> Result<(), SessionStoreError> {
    let owned = SESSION_SCHEMA_OBJECT_NAMES
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut allowed = HashSet::new();
    for name in allowed_foreign_objects {
        if name.is_empty() || owned.contains(name.as_str()) || !allowed.insert(name.as_str()) {
            return Err(SessionStoreError::IncompatibleSchema);
        }
    }
    Ok(())
}

fn validate_object_namespace(
    objects: &[(String, String, String, String)],
    allowed_foreign_objects: &[String],
) -> Result<(), SessionStoreError> {
    let owned = SESSION_SCHEMA_OBJECT_NAMES
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
        Err(SessionStoreError::IncompatibleSchema)
    }
}

fn owned_schema_objects(
    objects: Vec<(String, String, String, String)>,
) -> Vec<(String, String, String, String)> {
    let owned = SESSION_SCHEMA_OBJECT_NAMES
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    objects
        .into_iter()
        .filter(|object| owned.contains(object.1.as_str()))
        .collect()
}

fn validate_owned_foreign_keys(conn: &Connection) -> Result<(), SessionStoreError> {
    let owned = SESSION_SCHEMA_OBJECT_NAMES
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut statement = conn
        .prepare("PRAGMA foreign_key_check")
        .map_err(sqlite_err)?;
    let mut rows = statement.query([]).map_err(sqlite_err)?;
    while let Some(row) = rows.next().map_err(sqlite_err)? {
        let table = row.get::<_, String>(0).map_err(sqlite_err)?;
        if owned.contains(table.as_str()) {
            return Err(SessionStoreError::CorruptSchema);
        }
    }
    Ok(())
}

fn schema_objects(
    conn: &Connection,
) -> Result<Vec<(String, String, String, String)>, SessionStoreError> {
    let mut statement = conn
        .prepare(
            "SELECT type,name,tbl_name,sql FROM sqlite_schema \
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
        )
        .map_err(sqlite_err)?;
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(sqlite_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_err)
}

// ---------------------------------------------------------------------------
// Trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionStore for SqliteSessionStore {
    // ---- session metadata CRUD ----

    async fn list_persistent(
        &self,
        include_archived: bool,
    ) -> Result<Vec<StoredSession>, SessionStoreError> {
        // Boot-loader path: returns all persistent, non-archived
        // sessions across all users. The caller (runtime::boot) is
        // itself a system-actor that creates AgentRuns per session;
        // per-user filtering happens in `list` at request time.
        self.run(move |c| {
            let archive_filter = if include_archived {
                ""
            } else {
                " AND s.is_archived = 0"
            };
            let sql = format!(
                "SELECT s.id, s.name, s.lifetime, s.workspace, s.user_id, s.created_at, \
                        s.updated_at, s.external_meta, s.config_revision, s.config_overrides, \
                        s.effective_config, s.is_archived, GROUP_CONCAT(sa.agent_id, ',') AS agents \
                 FROM sessions s \
                 LEFT JOIN session_agents sa ON sa.session_id = s.id \
                 WHERE s.lifetime = 'persistent'{archive_filter} \
                 GROUP BY s.id \
                 ORDER BY s.updated_at DESC"
            );
            let mut stmt = c.prepare(&sql)?;
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
        overrides: sylvander_api::SessionConfigOverrides,
        effective: sylvander_api::SessionEffectiveConfig,
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
        ctx: &sylvander_api::SessionContext,
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
        let agent_instance_id = start.agent_instance_id.0.clone();
        if ctx.identity.agent_instance_id.as_ref() != Some(&start.agent_instance_id)
            || ctx.identity.agent_id != start.effective_config.agent_id
        {
            return Err(SessionStoreError::Invalid(
                "turn context does not own the requested Agent instance".into(),
            ));
        }
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
            let persisted: sylvander_api::SessionEffectiveConfig =
                decode_json(1, &stored_effective).map_err(sqlite_err)?;
            if persisted != start.effective_config {
                return Err(SessionStoreError::Invalid(
                    "turn configuration does not match the persisted session revision".into(),
                ));
            }
            let instance = transaction
                .query_row(
                    "SELECT agent_id,state FROM session_agent_instances \
                     WHERE session_id=?1 AND instance_id=?2",
                    params![start.session_id.0, start.agent_instance_id.0],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(sqlite_err)?;
            let Some((instance_agent_id, instance_state)) = instance else {
                return Err(SessionStoreError::Invalid(
                    "turn Agent instance is not a durable Session member".into(),
                ));
            };
            if instance_agent_id != start.effective_config.agent_id.0
                || matches!(
                    instance_state.as_str(),
                    "completed" | "failed" | "cancelled" | "manual_reconciliation"
                )
            {
                return Err(SessionStoreError::Invalid(
                    "turn Agent instance is unavailable or has the wrong definition".into(),
                ));
            }
            let unresolved_effect: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM session_tool_calls c \
                     JOIN session_turns t ON t.session_id=c.session_id AND t.turn_id=c.turn_id \
                     WHERE c.session_id=?1 AND t.agent_instance_id=?2 AND c.state='running')",
                    params![start.session_id.0, start.agent_instance_id.0],
                    |row| row.get(0),
                )
                .map_err(sqlite_err)?;
            if unresolved_effect {
                return Err(SessionStoreError::Invalid(
                    "Agent instance has an unresolved tool execution".into(),
                ));
            }

            let now = crate::session::now_secs();
            transaction
                .execute(
                    "INSERT INTO session_turns \
                     (session_id, turn_id, agent_instance_id, config_revision, effective_config, created_at, state) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running')",
                    params![
                        start.session_id.0,
                        start.turn_id,
                        start.agent_instance_id.0,
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
                     (session_id, seq, role, content_json, user_id, agent_id,agent_instance_id, \
                      trace_id, priority, model_id, is_summarized, created_at) \
                     VALUES (?1, ?2, 'user', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10)",
                    params![
                        start.session_id.0,
                        next_seq,
                        content_json,
                        user_id,
                        agent_id,
                        agent_instance_id,
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
                agent_instance_id: Some(start.agent_instance_id),
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

    async fn turn(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<Option<TurnSnapshot>, SessionStoreError> {
        let session_id = session_id.clone();
        let turn_id = turn_id.to_string();
        self.run(move |connection| {
            connection
                .query_row(
                    "SELECT agent_instance_id, config_revision, effective_config, created_at, state, ended_at, failure_kind \
                     FROM session_turns WHERE session_id = ?1 AND turn_id = ?2",
                    params![session_id.0, turn_id],
                    |row| {
                        let agent_instance_id: String = row.get(0)?;
                        let config_revision: i64 = row.get(1)?;
                        let effective: String = row.get(2)?;
                        Ok(TurnSnapshot {
                            session_id: session_id.clone(),
                            turn_id: turn_id.clone(),
                            agent_instance_id: AgentInstanceId::new(agent_instance_id),
                            config_revision: config_revision.try_into().map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    0,
                                    Type::Integer,
                                    Box::new(error),
                                )
                            })?,
                            effective_config: decode_json(2, &effective)?,
                            created_at: row.get(3)?,
                            state: decode_turn_state(row.get::<_, String>(4)?.as_str())?,
                            ended_at: row.get(5)?,
                            failure_kind: row
                                .get::<_, Option<String>>(6)?
                                .map(|value| decode_turn_failure_kind(&value))
                                .transpose()?,
                        })
                    },
                )
                .optional()
                .map_err(sqlite_err)
        })
        .await
    }

    async fn complete_turn(
        &self,
        ctx: &sylvander_api::SessionContext,
        completion: TurnCompletion,
    ) -> Result<StoredMessage, SessionStoreError> {
        let content_json = serde_json::to_string(&completion.assistant_content)
            .map_err(|error| SessionStoreError::Store(format!("serialize content: {error}")))?;
        let user_id = ctx.identity.user_id.0.clone();
        let agent_id = ctx.identity.agent_id.0.clone();
        let agent_instance_id = ctx.identity.agent_instance_id.clone();
        let trace_id = ctx.request.trace_id.clone();
        let priority = priority_str(ctx.request.priority);
        let stored_priority = Some(ctx.request.priority);
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction().map_err(sqlite_err)?;
            let turn: Option<(String, String)> = transaction
                .query_row(
                    "SELECT state,agent_instance_id FROM session_turns \
                     WHERE session_id = ?1 AND turn_id = ?2",
                    params![completion.session_id.0, completion.turn_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(sqlite_err)?;
            let Some((state, turn_instance_id)) = turn else {
                return Err(SessionStoreError::Invalid(
                    "durable turn does not exist".into(),
                ));
            };
            if state != "running" {
                return Err(SessionStoreError::Invalid(
                    "only a running turn can be completed".into(),
                ));
            }
            if agent_instance_id.as_ref().map(|id| id.0.as_str()) != Some(turn_instance_id.as_str())
            {
                return Err(SessionStoreError::Invalid(
                    "turn completion context does not own the durable Agent instance".into(),
                ));
            }
            let active_tools: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM session_tool_calls \
                     WHERE session_id = ?1 AND turn_id = ?2 AND state = 'running'",
                    params![completion.session_id.0, completion.turn_id],
                    |row| row.get(0),
                )
                .map_err(sqlite_err)?;
            if active_tools != 0 {
                return Err(SessionStoreError::Invalid(
                    "turn cannot complete while a durable tool call is running".into(),
                ));
            }
            let now = crate::session::now_secs();
            let next_seq: i64 = transaction
                .query_row(
                    "SELECT COALESCE(MAX(seq), -1) + 1 FROM session_messages WHERE session_id = ?1",
                    params![completion.session_id.0],
                    |row| row.get(0),
                )
                .map_err(sqlite_err)?;
            transaction
                .execute(
                    "INSERT INTO session_messages \
                     (session_id, seq, role, content_json, user_id, agent_id,agent_instance_id, \
                      trace_id, priority, model_id, is_summarized, created_at) \
                     VALUES (?1, ?2, 'assistant', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10)",
                    params![
                        completion.session_id.0,
                        next_seq,
                        content_json,
                        user_id,
                        agent_id,
                        agent_instance_id.as_ref().map(|id| &id.0),
                        trace_id,
                        priority,
                        completion.model_id,
                        now,
                    ],
                )
                .map_err(sqlite_err)?;
            let message_id = transaction.last_insert_rowid();
            transaction
                .execute(
                    "UPDATE session_turns SET state = 'completed', ended_at = ?3 \
                     WHERE session_id = ?1 AND turn_id = ?2 AND state = 'running'",
                    params![completion.session_id.0, completion.turn_id, now],
                )
                .map_err(sqlite_err)?;
            transaction
                .execute(
                    "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                    params![now, completion.session_id.0],
                )
                .map_err(sqlite_err)?;
            transaction.commit().map_err(sqlite_err)?;
            Ok(StoredMessage {
                id: message_id,
                session_id: completion.session_id,
                user_id: user_id.into(),
                agent_id: AgentId::new(agent_id),
                agent_instance_id,
                trace_id,
                priority: stored_priority,
                seq: next_seq.try_into().map_err(|_| {
                    SessionStoreError::Store("message sequence exceeds u32 range".into())
                })?,
                role: MessageRole::Assistant,
                content: completion.assistant_content,
                model_id: Some(completion.model_id),
                tool_name: None,
                parent_msg_id: None,
                is_summarized: false,
                created_at: now,
            })
        })
        .await
    }

    async fn complete_persisted_turn(
        &self,
        completion: PersistedTurnCompletion,
    ) -> Result<StoredMessage, SessionStoreError> {
        let expected = i64::try_from(completion.expected_revision).map_err(|_| {
            SessionStoreError::Invalid("model ledger revision exceeds SQLite range".into())
        })?;
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction().map_err(sqlite_err)?;
            let message = transaction
                .query_row(
                    "SELECT m.id,m.session_id,m.seq,m.role,m.content_json,m.user_id,m.agent_id,\
                            m.trace_id,m.priority,m.model_id,m.tool_name,m.parent_msg_id,\
                            m.is_summarized,m.created_at,m.agent_instance_id \
                     FROM session_turn_iterations i \
                     JOIN session_turns t ON t.session_id=i.session_id AND t.turn_id=i.turn_id \
                     JOIN session_messages m ON m.id=i.response_message_id \
                     WHERE i.invocation_id=?1 AND i.position='response_persisted' \
                       AND i.ledger_revision=?2 AND i.response_terminal=1 AND t.state='running' \
                       AND NOT EXISTS (SELECT 1 FROM session_tool_calls c \
                         WHERE c.session_id=i.session_id AND c.turn_id=i.turn_id \
                           AND c.state='running')",
                    params![completion.invocation_id.as_str(), expected],
                    row_to_message,
                )
                .optional()
                .map_err(sqlite_err)?
                .ok_or_else(|| {
                    SessionStoreError::Invalid(
                        "terminal response facts are missing, stale, or unresolved".into(),
                    )
                })?;
            let now = crate::session::now_secs();
            let changed = transaction
                .execute(
                    "UPDATE session_turn_iterations SET position='tools_resolved', \
                            ledger_revision=ledger_revision+1, updated_at=?2 \
                     WHERE invocation_id=?1 AND position='response_persisted' \
                       AND ledger_revision=?3 AND response_terminal=1",
                    params![completion.invocation_id.as_str(), now, expected],
                )
                .map_err(sqlite_err)?;
            if changed != 1 {
                return Err(SessionStoreError::Invalid(
                    "terminal model iteration CAS conflict".into(),
                ));
            }
            let changed = transaction
                .execute(
                    "UPDATE session_turns SET state='completed', ended_at=?3 \
                     WHERE session_id=?1 AND turn_id=(SELECT turn_id FROM session_turn_iterations \
                       WHERE invocation_id=?2) AND state='running'",
                    params![message.session_id.0, completion.invocation_id.as_str(), now],
                )
                .map_err(sqlite_err)?;
            if changed != 1 {
                return Err(SessionStoreError::Invalid(
                    "turn completion CAS conflict".into(),
                ));
            }
            transaction
                .execute(
                    "UPDATE sessions SET updated_at=?1 WHERE id=?2",
                    params![now, message.session_id.0],
                )
                .map_err(sqlite_err)?;
            transaction.commit().map_err(sqlite_err)?;
            Ok(message)
        })
        .await
    }

    async fn finish_turn(
        &self,
        session_id: &SessionId,
        turn_id: &str,
        state: TurnState,
        failure_kind: Option<TurnFailureKind>,
    ) -> Result<(), SessionStoreError> {
        let (state, failure_kind) = match (state, failure_kind) {
            (TurnState::Failed, Some(kind)) => ("failed", Some(turn_failure_kind_str(kind))),
            (TurnState::Interrupted, None) => ("interrupted", None),
            _ => {
                return Err(SessionStoreError::Invalid(
                    "finish_turn requires failed with a kind or interrupted without one".into(),
                ));
            }
        };
        let session_id = session_id.clone();
        let turn_id = turn_id.to_string();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction().map_err(sqlite_err)?;
            let now = crate::session::now_secs();
            let changed = transaction
                .execute(
                    "UPDATE session_turns SET state = ?3, ended_at = ?4, failure_kind = ?5 \
                     WHERE session_id = ?1 AND turn_id = ?2 AND state = 'running'",
                    params![session_id.0, turn_id, state, now, failure_kind],
                )
                .map_err(sqlite_err)?;
            if changed == 0 {
                return Err(SessionStoreError::Invalid(
                    "durable turn is missing or already terminal".into(),
                ));
            }
            transaction
                .execute(
                    "UPDATE session_tool_calls SET state = 'abandoned', ended_at = ?3 \
                     WHERE session_id = ?1 AND turn_id = ?2 AND state = 'running' \
                       AND position IN ('prepared','authorized')",
                    params![session_id.0, turn_id, now],
                )
                .map_err(sqlite_err)?;
            transaction.commit().map_err(sqlite_err)?;
            Ok(())
        })
        .await
    }

    async fn begin_model_iteration(
        &self,
        start: ModelIterationStart,
    ) -> Result<(), SessionStoreError> {
        if start.turn_id.trim().is_empty()
            || start.iteration == 0
            || start.model_id.trim().is_empty()
            || !start.capability_revision.starts_with("sha256:")
            || !start.request_digest.starts_with("sha256:")
        {
            return Err(SessionStoreError::Invalid(
                "model iteration identity or frozen request facts are invalid".into(),
            ));
        }
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction().map_err(sqlite_err)?;
            let turn_state: Option<String> = transaction
                .query_row(
                    "SELECT state FROM session_turns WHERE session_id=?1 AND turn_id=?2",
                    params![start.session_id.0, start.turn_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sqlite_err)?;
            if turn_state.as_deref() != Some("running") {
                return Err(SessionStoreError::Invalid(
                    "model iteration requires a running durable turn".into(),
                ));
            }
            let previous: Option<(i64, String)> = transaction
                .query_row(
                    "SELECT iteration, position FROM session_turn_iterations \
                     WHERE session_id=?1 AND turn_id=?2 ORDER BY iteration DESC LIMIT 1",
                    params![start.session_id.0, start.turn_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(sqlite_err)?;
            let expected_iteration = previous.as_ref().map_or(1, |(value, _)| value + 1);
            if i64::from(start.iteration) != expected_iteration
                || previous
                    .as_ref()
                    .is_some_and(|(_, position)| position != "tools_resolved")
            {
                return Err(SessionStoreError::Invalid(
                    "model iterations must be sequential and the previous iteration resolved"
                        .into(),
                ));
            }
            let now = crate::session::now_secs();
            transaction
                .execute(
                    "INSERT INTO session_turn_iterations \
                     (session_id, turn_id, iteration, invocation_id, model_id, \
                      capability_revision, request_digest, position, ledger_revision, \
                      started_at, updated_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,'model_started',0,?8,?8)",
                    params![
                        start.session_id.0,
                        start.turn_id,
                        start.iteration,
                        start.invocation_id.as_str(),
                        start.model_id,
                        start.capability_revision,
                        start.request_digest,
                        now,
                    ],
                )
                .map_err(|error| match error {
                    rusqlite::Error::SqliteFailure(ref inner, _)
                        if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        SessionStoreError::Invalid("conflicting model invocation identity".into())
                    }
                    error => sqlite_err(error),
                })?;
            transaction.commit().map_err(sqlite_err)
        })
        .await
    }

    async fn persist_model_response(
        &self,
        ctx: &sylvander_api::SessionContext,
        response: ModelResponsePersistence,
    ) -> Result<ModelResponseCommit, SessionStoreError> {
        let content_json = serde_json::to_string(&response.assistant_content)
            .map_err(|error| SessionStoreError::Store(format!("serialize content: {error}")))?;
        let user_id = ctx.identity.user_id.0.clone();
        let agent_id = ctx.identity.agent_id.0.clone();
        let agent_instance_id = ctx.identity.agent_instance_id.clone();
        let trace_id = ctx.request.trace_id.clone();
        let priority = priority_str(ctx.request.priority);
        let stored_priority = Some(ctx.request.priority);
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction().map_err(sqlite_err)?;
            let facts: Option<(String, String, i64, String, i64, String)> = transaction
                .query_row(
                    "SELECT i.session_id, i.model_id, i.ledger_revision, i.position, \
                            COALESCE(MAX(m.seq), -1) + 1, t.agent_instance_id \
                     FROM session_turn_iterations i \
                     JOIN session_turns t ON t.session_id=i.session_id AND t.turn_id=i.turn_id \
                     LEFT JOIN session_messages m ON m.session_id=i.session_id \
                     WHERE i.invocation_id=?1 AND t.state='running' \
                     GROUP BY i.session_id, i.model_id, i.ledger_revision, i.position, \
                              t.agent_instance_id",
                    [response.invocation_id.as_str()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .optional()
                .map_err(sqlite_err)?;
            let Some((session_id, frozen_model_id, revision, position, next_seq, turn_instance_id)) =
                facts
            else {
                return Err(SessionStoreError::Invalid(
                    "model response has no running durable invocation".into(),
                ));
            };
            if agent_instance_id.as_ref().map(|id| id.0.as_str())
                != Some(turn_instance_id.as_str())
            {
                return Err(SessionStoreError::Invalid(
                    "model response context does not own the durable Agent instance".into(),
                ));
            }
            if response.model_id != frozen_model_id
                || revision
                    != i64::try_from(response.expected_revision).map_err(|_| {
                        SessionStoreError::Invalid(
                            "model ledger revision exceeds SQLite range".into(),
                        )
                    })?
                || position != "model_started"
            {
                return Err(SessionStoreError::Invalid(
                    "stale model response persistence request".into(),
                ));
            }
            let now = crate::session::now_secs();
            transaction.execute(
                "INSERT INTO session_messages \
                 (session_id,seq,role,content_json,user_id,agent_id,agent_instance_id,trace_id,priority,model_id,is_summarized,created_at) \
                 VALUES (?1,?2,'assistant',?3,?4,?5,?6,?7,?8,?9,0,?10)",
                params![session_id, next_seq, content_json, user_id, agent_id,
                    agent_instance_id.as_ref().map(|id| &id.0), trace_id,
                    priority, response.model_id, now],
            ).map_err(sqlite_err)?;
            let message_id = transaction.last_insert_rowid();
            let changed = transaction.execute(
                "UPDATE session_turn_iterations SET position='response_persisted', \
                 ledger_revision=ledger_revision+1, response_message_id=?2, \
                 response_terminal=?3, updated_at=?4 \
                 WHERE invocation_id=?1 AND position='model_started' AND ledger_revision=?5",
                params![response.invocation_id.as_str(), message_id, response.terminal, now, revision],
            ).map_err(sqlite_err)?;
            if changed != 1 {
                return Err(SessionStoreError::Invalid("model response CAS conflict".into()));
            }
            transaction.commit().map_err(sqlite_err)?;
            Ok(ModelResponseCommit {
                message: StoredMessage {
                    id: message_id,
                    session_id: SessionId::new(session_id),
                    user_id: user_id.into(),
                    agent_id: AgentId::new(agent_id),
                    agent_instance_id,
                    trace_id,
                    priority: stored_priority,
                    seq: next_seq.try_into().map_err(|_| SessionStoreError::Store(
                        "message sequence exceeds u32 range".into(),
                    ))?,
                    role: MessageRole::Assistant,
                    content: response.assistant_content,
                    model_id: Some(response.model_id),
                    tool_name: None,
                    parent_msg_id: None,
                    is_summarized: false,
                    created_at: now,
                },
                ledger_revision: response.expected_revision + 1,
            })
        })
        .await
    }

    async fn advance_model_iteration(
        &self,
        advance: ModelIterationAdvance,
    ) -> Result<u64, SessionStoreError> {
        if !advance
            .expected_position
            .can_advance_to(advance.next_position)
        {
            return Err(SessionStoreError::Invalid(
                "model execution positions must advance one boundary".into(),
            ));
        }
        let expected = i64::try_from(advance.expected_revision).map_err(|_| {
            SessionStoreError::Invalid("model ledger revision exceeds SQLite range".into())
        })?;
        let next = advance
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| SessionStoreError::Invalid("model ledger revision overflow".into()))?;
        self.run(move |connection| {
            let changed = connection.execute(
                "UPDATE session_turn_iterations SET position=?1, ledger_revision=ledger_revision+1, \
                 updated_at=?2 WHERE invocation_id=?3 AND position=?4 AND ledger_revision=?5 \
                 AND NOT EXISTS (SELECT 1 FROM session_tool_calls c \
                   WHERE c.session_id=session_turn_iterations.session_id \
                     AND c.turn_id=session_turn_iterations.turn_id AND c.state='running')",
                params![model_position_str(advance.next_position), crate::session::now_secs(),
                    advance.invocation_id.as_str(), model_position_str(advance.expected_position), expected],
            ).map_err(sqlite_err)?;
            if changed == 1 {
                Ok(next)
            } else {
                Err(SessionStoreError::Invalid(
                    "model iteration CAS conflict or unresolved tool call".into(),
                ))
            }
        }).await
    }

    async fn model_iterations(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<Vec<ModelIterationSnapshot>, SessionStoreError> {
        query_model_iterations(self, Some((session_id.clone(), turn_id.to_owned()))).await
    }

    async fn interrupted_model_iterations(
        &self,
    ) -> Result<Vec<ModelIterationSnapshot>, SessionStoreError> {
        query_model_iterations(self, None).await
    }

    async fn classify_model_recovery(
        &self,
        write: ModelRecoveryWrite,
    ) -> Result<u64, SessionStoreError> {
        if write.recovery_owner.trim().is_empty() || write.lease_expires_at <= write.observed_at {
            return Err(SessionStoreError::Invalid(
                "model recovery lease is invalid".into(),
            ));
        }
        let expected = i64::try_from(write.expected_revision).map_err(|_| {
            SessionStoreError::Invalid("model ledger revision exceeds SQLite range".into())
        })?;
        let next = write
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| SessionStoreError::Invalid("model ledger revision overflow".into()))?;
        self.run(move |connection| {
            let changed = connection
                .execute(
                    "UPDATE session_turn_iterations SET ledger_revision=ledger_revision+1, \
                     recovery_decision=?1,recovery_reason=?2,operator_action_required=?3, \
                     recovery_attempts=recovery_attempts+1,recovery_owner=?4, \
                     recovery_lease_expires_at=?5,first_interrupted_at=COALESCE(first_interrupted_at,?6), \
                     updated_at=?6 WHERE invocation_id=?7 AND ledger_revision=?8 \
                     AND (recovery_lease_expires_at IS NULL OR recovery_lease_expires_at<=?6 \
                          OR recovery_owner=?4)",
                    params![
                        model_recovery_decision_str(write.classification.decision),
                        model_recovery_reason_str(write.classification.reason),
                        write.classification.operator_action_required,
                        write.recovery_owner,
                        write.lease_expires_at,
                        write.observed_at,
                        write.invocation_id.as_str(),
                        expected,
                    ],
                )
                .map_err(sqlite_err)?;
            if changed == 1 {
                Ok(next)
            } else {
                Err(SessionStoreError::Invalid(
                    "model recovery lease or ledger CAS conflict".into(),
                ))
            }
        })
        .await
    }

    async fn begin_tool_call(&self, start: ToolCallStart) -> Result<(), SessionStoreError> {
        if start.turn_id.trim().is_empty()
            || start.call_id.trim().is_empty()
            || start.tool_name.trim().is_empty()
            || !start.capability_revision.starts_with("sha256:")
            || !start.input_digest.starts_with("sha256:")
            || !recovery_policy_allows(
                start.declared_recovery_policy,
                start.effective_recovery_policy,
            )
        {
            return Err(SessionStoreError::Invalid(
                "tool execution identity or frozen recovery contract is invalid".into(),
            ));
        }
        self.run(move |connection| {
            let turn_state = connection
                .query_row(
                    "SELECT state FROM session_turns WHERE session_id = ?1 AND turn_id = ?2",
                    params![start.session_id.0, start.turn_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sqlite_err)?;
            if turn_state.as_deref() != Some("running") {
                return Err(SessionStoreError::Invalid(
                    "tool call requires a running durable turn".into(),
                ));
            }
            let now = crate::session::now_secs();
            let changed = connection.execute(
                    "INSERT INTO session_tool_calls \
                     (session_id, turn_id, call_id, invocation_id, tool_name, invocation_class, \
                      declared_recovery_policy, effective_recovery_policy, capability_revision, \
                      input_digest, position, ledger_revision, started_at, updated_at, state) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'prepared', 0, ?11, ?11, 'running') \
                     ON CONFLICT(session_id, turn_id, call_id) DO NOTHING",
                    params![
                        &start.session_id.0,
                        &start.turn_id,
                        &start.call_id,
                        start.invocation_id.as_str(),
                        &start.tool_name,
                        start.invocation_class.map(tool_invocation_class_str),
                        tool_recovery_policy_str(start.declared_recovery_policy),
                        tool_recovery_policy_str(start.effective_recovery_policy),
                        &start.capability_revision,
                        &start.input_digest,
                        now,
                    ],
                )
                .map_err(|error| match error {
                    rusqlite::Error::SqliteFailure(ref inner, _)
                        if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        SessionStoreError::Invalid("conflicting durable invocation identity".into())
                    }
                    error => sqlite_err(error),
                })?;
            if changed == 1 {
                return Ok(());
            }
            let existing = connection
                .query_row(
                    "SELECT invocation_id, tool_name, invocation_class, \
                            declared_recovery_policy, effective_recovery_policy, \
                            capability_revision, input_digest \
                     FROM session_tool_calls \
                     WHERE session_id = ?1 AND turn_id = ?2 AND call_id = ?3",
                    params![start.session_id.0, start.turn_id, start.call_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(sqlite_err)?;
            let expected_class = start.invocation_class.map(tool_invocation_class_str);
            match existing {
                Some((invocation_id, tool_name, class, declared, effective, revision, digest))
                    if invocation_id == start.invocation_id.as_str()
                        && tool_name == start.tool_name
                        && class.as_deref() == expected_class
                        && declared == tool_recovery_policy_str(start.declared_recovery_policy)
                        && effective == tool_recovery_policy_str(start.effective_recovery_policy)
                        && revision == start.capability_revision
                        && digest == start.input_digest =>
                {
                    Ok(())
                }
                _ => Err(SessionStoreError::Invalid(
                    "conflicting durable tool call fingerprint".into(),
                )),
            }
        })
        .await
    }

    async fn advance_tool_call(&self, advance: ToolCallAdvance) -> Result<u64, SessionStoreError> {
        if !advance
            .expected_position
            .can_advance_to(advance.next_position)
        {
            return Err(SessionStoreError::Invalid(
                "tool execution position must advance one boundary".into(),
            ));
        }
        self.run(move |connection| {
            let next_revision = advance.expected_revision.checked_add(1).ok_or_else(|| {
                SessionStoreError::Invalid("tool ledger revision overflow".into())
            })?;
            let expected_revision_sql = i64::try_from(advance.expected_revision).map_err(|_| {
                SessionStoreError::Invalid("tool ledger revision exceeds SQLite range".into())
            })?;
            let next_revision_sql = i64::try_from(next_revision).map_err(|_| {
                SessionStoreError::Invalid("tool ledger revision exceeds SQLite range".into())
            })?;
            let changed = connection
                .execute(
                    "UPDATE session_tool_calls \
                     SET position = ?6, ledger_revision = ?5, updated_at = ?7 \
                     WHERE session_id = ?1 AND turn_id = ?2 AND call_id = ?3 \
                       AND ledger_revision = ?4 AND position = ?8 AND state = 'running'",
                    params![
                        advance.session_id.0,
                        advance.turn_id,
                        advance.call_id,
                        expected_revision_sql,
                        next_revision_sql,
                        tool_execution_position_str(advance.next_position),
                        crate::session::now_secs(),
                        tool_execution_position_str(advance.expected_position),
                    ],
                )
                .map_err(sqlite_err)?;
            if changed == 1 {
                return Ok(next_revision);
            }
            let existing = connection
                .query_row(
                    "SELECT position, ledger_revision FROM session_tool_calls \
                     WHERE session_id = ?1 AND turn_id = ?2 AND call_id = ?3",
                    params![advance.session_id.0, advance.turn_id, advance.call_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(sqlite_err)?;
            match existing {
                Some((position, revision))
                    if position == tool_execution_position_str(advance.next_position)
                        && revision == next_revision_sql =>
                {
                    Ok(next_revision)
                }
                _ => Err(SessionStoreError::Invalid(
                    "tool execution position compare-and-swap conflict".into(),
                )),
            }
        })
        .await
    }

    async fn persist_tool_result(
        &self,
        ctx: &sylvander_api::SessionContext,
        result: ToolResultPersistence,
    ) -> Result<u64, SessionStoreError> {
        let (terminal_state, failure_kind) = match (result.terminal_state, result.failure_kind) {
            (ToolCallState::Succeeded, None) => ("succeeded", None),
            (ToolCallState::Failed, kind) => ("failed", kind.map(tool_call_failure_kind_str)),
            _ => {
                return Err(SessionStoreError::Invalid(
                    "tool result terminal state and failure kind are inconsistent".into(),
                ));
            }
        };
        if result.tool_name.trim().is_empty()
            || !matches!(
                result.expected_position,
                ToolExecutionPosition::EffectStarted | ToolExecutionPosition::EffectCommitted
            )
        {
            return Err(SessionStoreError::Invalid(
                "tool result persistence boundary is invalid".into(),
            ));
        }
        let content_json = serde_json::to_string(&result.content)
            .map_err(|error| SessionStoreError::Store(format!("serialize content: {error}")))?;
        let user_id = ctx.identity.user_id.0.clone();
        let agent_id = ctx.identity.agent_id.0.clone();
        let agent_instance_id = ctx.identity.agent_instance_id.clone();
        let trace_id = ctx.request.trace_id.clone();
        let priority = priority_str(ctx.request.priority);
        self.run(move |connection| {
            let expected_revision = i64::try_from(result.expected_revision).map_err(|_| {
                SessionStoreError::Invalid("tool ledger revision exceeds SQLite range".into())
            })?;
            let next_revision = expected_revision.checked_add(1).ok_or_else(|| {
                SessionStoreError::Invalid("tool ledger revision overflow".into())
            })?;
            let transaction = connection.unchecked_transaction().map_err(sqlite_err)?;
            let turn_instance_id: Option<String> = transaction
                .query_row(
                    "SELECT agent_instance_id FROM session_turns \
                     WHERE session_id=?1 AND turn_id=?2 AND state='running'",
                    params![result.session_id.0, result.turn_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sqlite_err)?;
            if agent_instance_id.as_ref().map(|id| id.0.as_str()) != turn_instance_id.as_deref() {
                return Err(SessionStoreError::Invalid(
                    "tool result context does not own the durable Agent instance".into(),
                ));
            }
            let now = crate::session::now_secs();
            let advanced = transaction
                .execute(
                    "UPDATE session_tool_calls \
                     SET position='result_persisted', ledger_revision=?5, updated_at=?6, \
                         state=?8, ended_at=?6, failure_kind=?9 \
                     WHERE session_id=?1 AND turn_id=?2 AND call_id=?3 \
                       AND ledger_revision=?4 AND position=?7 AND state='running'",
                    params![
                        result.session_id.0,
                        result.turn_id,
                        result.call_id,
                        expected_revision,
                        next_revision,
                        now,
                        tool_execution_position_str(result.expected_position),
                        terminal_state,
                        failure_kind,
                    ],
                )
                .map_err(sqlite_err)?;
            if advanced != 1 {
                return Err(SessionStoreError::Invalid(
                    "tool result position compare-and-swap conflict".into(),
                ));
            }
            let next_seq = transaction
                .query_row(
                    "SELECT COALESCE(MAX(seq), -1) + 1 FROM session_messages \
                     WHERE session_id=?1",
                    [&result.session_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(sqlite_err)?;
            transaction
                .execute(
                    "INSERT INTO session_messages \
                     (session_id,seq,role,content_json,user_id,agent_id,agent_instance_id, \
                      trace_id,priority,tool_name,is_summarized,created_at) \
                     VALUES (?1,?2,'tool',?3,?4,?5,?6,?7,?8,?9,0,?10)",
                    params![
                        result.session_id.0,
                        next_seq,
                        content_json,
                        user_id,
                        agent_id,
                        agent_instance_id.as_ref().map(|id| &id.0),
                        trace_id,
                        priority,
                        result.tool_name,
                        now,
                    ],
                )
                .map_err(sqlite_err)?;
            transaction.commit().map_err(sqlite_err)?;
            u64::try_from(next_revision)
                .map_err(|_| SessionStoreError::Invalid("negative tool ledger revision".into()))
        })
        .await
    }

    async fn finish_tool_call(
        &self,
        completion: ToolCallCompletion,
    ) -> Result<(), SessionStoreError> {
        let (state, failure_kind) = match (completion.state, completion.failure_kind) {
            (ToolCallState::Succeeded, None) => ("succeeded", None),
            (ToolCallState::Failed, kind) => ("failed", kind.map(tool_call_failure_kind_str)),
            (ToolCallState::Rejected, None) => ("rejected", None),
            _ => {
                return Err(SessionStoreError::Invalid(
                    "tool terminal state and failure kind are inconsistent".into(),
                ));
            }
        };
        self.run(move |connection| {
            let changed = connection
                .execute(
                    "UPDATE session_tool_calls SET state = ?4, ended_at = ?5, failure_kind = ?6 \
                     WHERE session_id = ?1 AND turn_id = ?2 AND call_id = ?3 AND state = 'running'",
                    params![
                        completion.session_id.0,
                        completion.turn_id,
                        completion.call_id,
                        state,
                        crate::session::now_secs(),
                        failure_kind
                    ],
                )
                .map_err(sqlite_err)?;
            if changed == 0 {
                return Err(SessionStoreError::Invalid(
                    "durable tool call is missing or already terminal".into(),
                ));
            }
            Ok(())
        })
        .await
    }

    async fn tool_calls(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<Vec<ToolCallSnapshot>, SessionStoreError> {
        let session_id = session_id.clone();
        let turn_id = turn_id.to_owned();
        self.run(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT call_id, invocation_id, tool_name, invocation_class, \
                            declared_recovery_policy, effective_recovery_policy, \
                            capability_revision, input_digest, position, ledger_revision, \
                            started_at, updated_at, state, ended_at, failure_kind, \
                            recovery_decision, recovery_reason, operator_action_required, \
                            recovery_attempts, recovery_owner, recovery_lease_expires_at, \
                            first_interrupted_at \
                     FROM session_tool_calls WHERE session_id = ?1 AND turn_id = ?2 \
                     ORDER BY started_at, call_id",
                )
                .map_err(sqlite_err)?;
            statement
                .query_map(params![session_id.0, turn_id], |row| {
                    decode_tool_call_row(row, session_id.clone(), turn_id.clone(), 0)
                })
                .map_err(sqlite_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_err)
        })
        .await
    }

    async fn interrupted_tool_calls(&self) -> Result<Vec<ToolCallSnapshot>, SessionStoreError> {
        self.run(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT calls.session_id, calls.turn_id, calls.call_id, calls.invocation_id, \
                            calls.tool_name, calls.invocation_class, \
                            calls.declared_recovery_policy, calls.effective_recovery_policy, \
                            calls.capability_revision, calls.input_digest, calls.position, \
                            calls.ledger_revision, calls.started_at, calls.updated_at, calls.state, \
                            calls.ended_at, calls.failure_kind, calls.recovery_decision, \
                            calls.recovery_reason, calls.operator_action_required, \
                            calls.recovery_attempts, calls.recovery_owner, \
                            calls.recovery_lease_expires_at, calls.first_interrupted_at \
                     FROM session_tool_calls AS calls \
                     JOIN session_turns AS turns \
                       ON turns.session_id = calls.session_id AND turns.turn_id = calls.turn_id \
                     WHERE calls.state = 'running' \
                     ORDER BY calls.started_at, calls.invocation_id",
                )
                .map_err(sqlite_err)?;
            statement
                .query_map([], |row| {
                    decode_tool_call_row(row, SessionId::new(row.get::<_, String>(0)?), row.get(1)?, 2)
                })
                .map_err(sqlite_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_err)
        })
        .await
    }

    async fn classify_tool_recovery(
        &self,
        write: ToolRecoveryWrite,
    ) -> Result<u64, SessionStoreError> {
        if write.recovery_owner.trim().is_empty() || write.lease_expires_at <= write.observed_at {
            return Err(SessionStoreError::Invalid(
                "recovery owner and lease interval are invalid".into(),
            ));
        }
        self.run(move |connection| {
            let expected = i64::try_from(write.expected_revision).map_err(|_| {
                SessionStoreError::Invalid("tool ledger revision exceeds SQLite range".into())
            })?;
            let next = expected.checked_add(1).ok_or_else(|| {
                SessionStoreError::Invalid("tool ledger revision overflow".into())
            })?;
            let changed = connection
                .execute(
                    "UPDATE session_tool_calls \
                     SET recovery_decision = ?3, recovery_reason = ?4, \
                         operator_action_required = ?5, recovery_attempts = recovery_attempts + 1, \
                         recovery_owner = ?6, recovery_lease_expires_at = ?7, \
                         first_interrupted_at = COALESCE(first_interrupted_at, ?8), \
                         ledger_revision = ?9, updated_at = ?8 \
                     WHERE invocation_id = ?1 AND ledger_revision = ?2 AND state = 'running' \
                       AND (recovery_owner IS NULL OR recovery_owner = ?6 \
                            OR recovery_lease_expires_at <= ?8)",
                    params![
                        write.invocation_id.as_str(),
                        expected,
                        tool_recovery_decision_str(write.classification.decision),
                        tool_recovery_reason_str(write.classification.reason),
                        write.classification.operator_action_required,
                        write.recovery_owner,
                        write.lease_expires_at,
                        write.observed_at,
                        next,
                    ],
                )
                .map_err(sqlite_err)?;
            if changed == 1 {
                u64::try_from(next)
                    .map_err(|_| SessionStoreError::Invalid("negative tool ledger revision".into()))
            } else {
                Err(SessionStoreError::Invalid(
                    "tool recovery lease or revision compare-and-swap conflict".into(),
                ))
            }
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
                        s.config_overrides, s.effective_config, s.is_archived, \
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

    async fn get_including_archived(
        &self,
        id: &SessionId,
    ) -> Result<Option<StoredSession>, SessionStoreError> {
        let id = id.clone();
        self.run(move |c| {
            let mut stmt = c.prepare(
                "SELECT s.id, s.name, s.lifetime, s.workspace, s.user_id, \
                        s.created_at, s.updated_at, s.external_meta, s.config_revision, \
                        s.config_overrides, s.effective_config, s.is_archived, \
                        GROUP_CONCAT(sa.agent_id, ',') AS agents \
                 FROM sessions s \
                 LEFT JOIN session_agents sa ON sa.session_id = s.id \
                 WHERE s.id = ?1 \
                 GROUP BY s.id",
            )?;
            stmt.query_row(params![id.0], row_to_session_with_agents)
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    async fn list(
        &self,
        ctx: &sylvander_api::SessionContext,
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
                        s.config_overrides, s.effective_config, s.is_archived, \
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
        ctx: &sylvander_api::SessionContext,
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
                        config_revision, config_overrides, effective_config, is_archived \
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
        ctx: &sylvander_api::SessionContext,
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
        let agent_instance_id = ctx.identity.agent_instance_id.clone();
        let trace_id = ctx.request.trace_id.clone();
        let priority = Some(priority_str(ctx.request.priority));
        let now = crate::session::now_secs();

        self.run(move |c| {
            // Verify session exists (and isn't archived) before insert.
            let exists: Option<i64> = c
                .query_row(
                    "SELECT 1 FROM sessions s WHERE s.id = ?1 AND s.is_archived = 0 \
                     AND s.user_id = ?2 AND (?3 IS NULL OR EXISTS (\
                       SELECT 1 FROM session_agent_instances a \
                       WHERE a.session_id=s.id AND a.instance_id=?3 AND a.agent_id=?4))",
                    params![
                        session_id.0,
                        user_id,
                        agent_instance_id.as_ref().map(|id| &id.0),
                        agent_id
                    ],
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
                 (session_id, seq, role, content_json, user_id, agent_id,agent_instance_id, \
                  trace_id, priority, model_id, tool_name,parent_msg_id,is_summarized,created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,0,?13)",
                params![
                    session_id.0,
                    next_seq,
                    role_str,
                    content_json,
                    user_id,
                    agent_id,
                    agent_instance_id.as_ref().map(|id| &id.0),
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
                            is_summarized, created_at, agent_instance_id \
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
        ctx: &sylvander_api::SessionContext,
        session_id: &SessionId,
        include_summarized: bool,
        limit: Option<usize>,
    ) -> Result<Vec<StoredMessage>, SessionStoreError> {
        let session_id = session_id.clone();
        let scope_user = ctx.identity.user_id.0.clone();
        let scope_agent = ctx.identity.agent_id.0.clone();
        let scope_instance = ctx
            .identity
            .agent_instance_id
            .as_ref()
            .map(|id| id.0.clone());
        self.run(move |c| {
            let mut sql = String::from(
                "SELECT id, session_id, seq, role, content_json, \
                        user_id, agent_id, trace_id, priority, \
                        model_id, tool_name, parent_msg_id, \
                        is_summarized, created_at, agent_instance_id \
                 FROM session_messages \
                 WHERE session_id = ?1 AND user_id = ?2 \
                   AND ((?3 IS NULL AND agent_instance_id IS NULL) \
                        OR agent_instance_id = ?3) \
                   AND (?3 IS NULL OR EXISTS (SELECT 1 FROM session_agent_instances a \
                     WHERE a.session_id=?1 AND a.instance_id=?3 AND a.agent_id=?4))",
            );
            if !include_summarized {
                sql.push_str(" AND is_summarized = 0");
            }
            sql.push_str(" ORDER BY seq ASC");
            if let Some(limit) = limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(
                params![session_id.0, scope_user, scope_instance, scope_agent],
                row_to_message,
            )?;
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
        ctx: &sylvander_api::SessionContext,
        session_id: &SessionId,
        seq_range: Range<u32>,
    ) -> Result<(), SessionStoreError> {
        let session_id = session_id.clone();
        let scope_user = ctx.identity.user_id.0.clone();
        let scope_agent = ctx.identity.agent_id.0.clone();
        let scope_instance = ctx
            .identity
            .agent_instance_id
            .as_ref()
            .map(|id| id.0.clone());
        self.run(move |c| {
            // Range is half-open: start inclusive, end exclusive.
            c.execute(
                "UPDATE session_messages SET is_summarized = 1 \
                 WHERE session_id = ?1 AND user_id = ?2 \
                   AND ((?3 IS NULL AND agent_instance_id IS NULL) \
                        OR agent_instance_id = ?3) \
                   AND (?3 IS NULL OR EXISTS (SELECT 1 FROM session_agent_instances a \
                     WHERE a.session_id=?1 AND a.instance_id=?3 AND a.agent_id=?4)) \
                   AND seq >= ?5 AND seq < ?6",
                params![
                    session_id.0,
                    scope_user,
                    scope_instance,
                    scope_agent,
                    seq_range.start,
                    seq_range.end
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn replace_active_history(
        &self,
        ctx: &sylvander_api::SessionContext,
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
        let agent_instance_id = ctx.identity.agent_instance_id.clone();
        let trace_id = ctx.request.trace_id.clone();
        let priority = priority_str(ctx.request.priority);
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction().map_err(sqlite_err)?;
            let exists: Option<i64> = transaction
                .query_row(
                    "SELECT 1 FROM sessions s WHERE s.id = ?1 AND s.is_archived = 0 \
                     AND s.user_id=?2 AND (?3 IS NULL OR EXISTS (\
                       SELECT 1 FROM session_agent_instances a \
                       WHERE a.session_id=s.id AND a.instance_id=?3 AND a.agent_id=?4))",
                    params![
                        session_id.0,
                        user_id,
                        agent_instance_id.as_ref().map(|id| &id.0),
                        agent_id
                    ],
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
                     WHERE session_id = ?1 AND user_id = ?2 \
                       AND ((?3 IS NULL AND agent_instance_id IS NULL) \
                            OR agent_instance_id = ?3) \
                       AND is_summarized = 0",
                    params![
                        session_id.0,
                        user_id,
                        agent_instance_id.as_ref().map(|id| &id.0)
                    ],
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
                          agent_instance_id,trace_id,priority,tool_name,is_summarized,created_at) \
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0,?11)",
                        params![
                            session_id.0,
                            next_seq,
                            role,
                            content,
                            user_id,
                            agent_id,
                            agent_instance_id.as_ref().map(|id| &id.0),
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
        ctx: &sylvander_api::SessionContext,
        session_id: &SessionId,
    ) -> Result<u64, SessionStoreError> {
        let session_id = session_id.clone();
        let scope_user = ctx.identity.user_id.0.clone();
        let scope_agent = ctx.identity.agent_id.0.clone();
        let scope_instance = ctx
            .identity
            .agent_instance_id
            .as_ref()
            .map(|id| id.0.clone());
        self.run(move |c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM session_messages \
                 WHERE session_id = ?1 \
                   AND user_id = ?2 \
                   AND ((?3 IS NULL AND agent_instance_id IS NULL) \
                        OR agent_instance_id = ?3) \
                   AND (?3 IS NULL OR EXISTS (SELECT 1 FROM session_agent_instances a \
                     WHERE a.session_id=?1 AND a.instance_id=?3 AND a.agent_id=?4)) \
                   AND is_summarized = 0",
                params![session_id.0, scope_user, scope_instance, scope_agent],
                |r| r.get(0),
            )?;
            Ok(n as u64)
        })
        .await
    }

    async fn materialize_agent_fork_history(
        &self,
        session_id: &SessionId,
        parent_instance_id: &AgentInstanceId,
        child_instance_id: &AgentInstanceId,
        base_sequence: u64,
        now: i64,
    ) -> Result<u64, SessionStoreError> {
        if parent_instance_id == child_instance_id {
            return Err(SessionStoreError::Invalid(
                "fork history parent and child must differ".into(),
            ));
        }
        let session_id = session_id.clone();
        let parent = parent_instance_id.clone();
        let child = child_instance_id.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let copied = materialize_fork_history(
                &transaction,
                &session_id,
                &parent,
                &child,
                base_sequence,
                now,
            )?;
            transaction.commit()?;
            Ok(copied)
        })
        .await
    }
}

pub(crate) fn materialize_fork_history(
    transaction: &rusqlite::Transaction<'_>,
    session_id: &SessionId,
    parent: &AgentInstanceId,
    child: &AgentInstanceId,
    base_sequence: u64,
    now: i64,
) -> Result<u64, SessionStoreError> {
    let existing = transaction
        .query_row(
            "SELECT session_id,parent_instance_id,base_sequence,copied_messages \
             FROM agent_history_fork_receipts WHERE child_instance_id=?1",
            [&child.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing.0 == session_id.0
            && existing.1 == parent.0
            && session_u64(existing.2, "fork base sequence")? == base_sequence
        {
            return session_u64(existing.3, "fork copied message count");
        }
        return Err(SessionStoreError::Invalid(
            "fork history receipt conflicts with requested intent".into(),
        ));
    }
    let child_message_count = transaction.query_row(
        "SELECT COUNT(*) FROM session_messages WHERE session_id=?1 \
                 AND agent_instance_id=?2",
        params![session_id.0, child.0],
        |row| row.get::<_, i64>(0),
    )?;
    if child_message_count != 0 {
        return Err(SessionStoreError::Invalid(
            "fork child history exists without a durable receipt".into(),
        ));
    }
    let parent_exists = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM session_agent_instances WHERE session_id=?1 \
                 AND instance_id=?2)",
        params![session_id.0, parent.0],
        |row| row.get::<_, bool>(0),
    )?;
    if !parent_exists {
        return Err(SessionStoreError::Invalid(
            "fork history parent does not exist".into(),
        ));
    }
    let base = session_i64(base_sequence, "fork base sequence")?;
    let source_rows = transaction.query_row(
        "SELECT COUNT(*) FROM session_messages WHERE session_id=?1 \
                 AND agent_instance_id=?2 AND seq<?3 AND is_summarized=0",
        params![session_id.0, parent.0, base],
        |row| row.get::<_, i64>(0),
    )?;
    let mut next_seq = transaction.query_row(
        "SELECT COALESCE(MAX(seq),-1)+1 FROM session_messages WHERE session_id=?1",
        [&session_id.0],
        |row| row.get::<_, i64>(0),
    )?;
    let mut statement = transaction.prepare(
        "SELECT role,content_json,user_id,agent_id,trace_id,priority,model_id,tool_name,
                        created_at FROM session_messages WHERE session_id=?1 \
                 AND agent_instance_id=?2 AND seq<?3 AND is_summarized=0 ORDER BY seq",
    )?;
    let rows = statement
        .query_map(params![session_id.0, parent.0, base], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, i64>(8)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for row in rows {
        transaction.execute(
                    "INSERT INTO session_messages(session_id,seq,role,content_json,user_id,agent_id,
                     agent_instance_id,trace_id,priority,model_id,tool_name,parent_msg_id,
                     is_summarized,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,NULL,0,?12)",
                    params![
                        session_id.0,
                        next_seq,
                        row.0,
                        row.1,
                        row.2,
                        row.3,
                        child.0,
                        row.4,
                        row.5,
                        row.6,
                        row.7,
                        row.8,
                    ],
                )?;
        next_seq = next_seq
            .checked_add(1)
            .ok_or_else(|| SessionStoreError::Invalid("message sequence overflow".into()))?;
    }
    transaction.execute(
        "INSERT INTO agent_history_fork_receipts(child_instance_id,session_id,
                 parent_instance_id,base_sequence,copied_messages,materialized_at) \
                 VALUES (?1,?2,?3,?4,?5,?6)",
        params![child.0, session_id.0, parent.0, base, source_rows, now],
    )?;
    session_u64(source_rows, "fork copied message count")
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

fn session_i64(value: u64, label: &str) -> Result<i64, SessionStoreError> {
    value
        .try_into()
        .map_err(|_| SessionStoreError::Invalid(format!("{label} exceeds SQLite range")))
}

fn session_u64(value: i64, label: &str) -> Result<u64, SessionStoreError> {
    value
        .try_into()
        .map_err(|_| SessionStoreError::Store(format!("stored {label} is negative")))
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

async fn query_model_iterations(
    store: &SqliteSessionStore,
    turn: Option<(SessionId, String)>,
) -> Result<Vec<ModelIterationSnapshot>, SessionStoreError> {
    store
        .run(move |connection| {
            let selected = "SELECT i.session_id,i.turn_id,i.iteration,i.invocation_id,i.model_id,\
                i.capability_revision,i.request_digest,i.position,i.ledger_revision,\
                CASE WHEN m.session_id=i.session_id AND m.role='assistant' THEN m.id END,\
                i.response_terminal,i.started_at,i.updated_at,i.recovery_decision,\
                i.recovery_reason,i.operator_action_required,i.recovery_attempts,\
                i.recovery_owner,i.recovery_lease_expires_at,i.first_interrupted_at \
                FROM session_turn_iterations i \
                LEFT JOIN session_messages m ON m.id=i.response_message_id";
            let mut snapshots = Vec::new();
            if let Some((session_id, turn_id)) = turn {
                let sql = format!(
                    "{selected} WHERE i.session_id=?1 AND i.turn_id=?2 ORDER BY i.iteration"
                );
                let mut statement = connection.prepare(&sql).map_err(sqlite_err)?;
                let rows = statement
                    .query_map(params![session_id.0, turn_id], decode_model_iteration_row)
                    .map_err(sqlite_err)?;
                for row in rows {
                    snapshots.push(row.map_err(sqlite_err)?);
                }
            } else {
                let sql = format!(
                    "{selected} JOIN session_turns t ON t.session_id=i.session_id \
                     AND t.turn_id=i.turn_id WHERE t.state='running' AND NOT EXISTS (\
                       SELECT 1 FROM session_turn_iterations later \
                       WHERE later.session_id=i.session_id AND later.turn_id=i.turn_id \
                         AND later.iteration>i.iteration) ORDER BY i.updated_at,i.invocation_id"
                );
                let mut statement = connection.prepare(&sql).map_err(sqlite_err)?;
                let rows = statement
                    .query_map([], decode_model_iteration_row)
                    .map_err(sqlite_err)?;
                for row in rows {
                    snapshots.push(row.map_err(sqlite_err)?);
                }
            }
            Ok(snapshots)
        })
        .await
}

fn decode_model_iteration_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelIterationSnapshot> {
    let iteration: i64 = row.get(2)?;
    let invocation_id = ModelInvocationId::parse(row.get(3)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, Type::Text, Box::new(error))
    })?;
    let ledger_revision: i64 = row.get(8)?;
    Ok(ModelIterationSnapshot {
        session_id: SessionId::new(row.get::<_, String>(0)?),
        turn_id: row.get(1)?,
        iteration: iteration.try_into().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(2, Type::Integer, Box::new(error))
        })?,
        invocation_id,
        model_id: row.get(4)?,
        capability_revision: row.get(5)?,
        request_digest: row.get(6)?,
        position: decode_model_position(&row.get::<_, String>(7)?)?,
        ledger_revision: ledger_revision.try_into().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(8, Type::Integer, Box::new(error))
        })?,
        response_message_id: row.get(9)?,
        response_terminal: row.get(10)?,
        started_at: row.get(11)?,
        updated_at: row.get(12)?,
        recovery_decision: row
            .get::<_, Option<String>>(13)?
            .map(|value| decode_model_recovery_decision(&value))
            .transpose()?,
        recovery_reason: row
            .get::<_, Option<String>>(14)?
            .map(|value| decode_model_recovery_reason(&value))
            .transpose()?,
        operator_action_required: row.get(15)?,
        recovery_attempts: row.get::<_, i64>(16)?.try_into().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(16, Type::Integer, Box::new(error))
        })?,
        recovery_owner: row.get(17)?,
        recovery_lease_expires_at: row.get(18)?,
        first_interrupted_at: row.get(19)?,
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
    let archived: bool = row.get(11)?;

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
        archived,
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
    let archived: bool = row.get(11)?;
    let agents_csv: Option<String> = row.get(12)?;

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
        archived,
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
    let agent_instance_id: Option<String> = row.get(14)?;

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
        user_id: sylvander_api::UserId::new(user_id),
        agent_id: sylvander_api::AgentId::new(agent_id),
        agent_instance_id: agent_instance_id.map(AgentInstanceId::new),
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

fn priority_str(p: sylvander_api::session_context::Priority) -> String {
    match p {
        Priority::Low => "low",
        Priority::Normal => "normal",
        Priority::High => "high",
        Priority::Urgent => "urgent",
    }
    .to_string()
}

fn turn_failure_kind_str(kind: TurnFailureKind) -> &'static str {
    match kind {
        TurnFailureKind::UnknownSession => "unknown_session",
        TurnFailureKind::Authentication => "authentication",
        TurnFailureKind::AgentLoop => "agent_loop",
        TurnFailureKind::Configuration => "configuration",
        TurnFailureKind::Persistence => "persistence",
    }
}

fn decode_turn_state(value: &str) -> rusqlite::Result<TurnState> {
    match value {
        "running" => Ok(TurnState::Running),
        "completed" => Ok(TurnState::Completed),
        "failed" => Ok(TurnState::Failed),
        "interrupted" => Ok(TurnState::Interrupted),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn decode_turn_failure_kind(value: &str) -> rusqlite::Result<TurnFailureKind> {
    match value {
        "unknown_session" => Ok(TurnFailureKind::UnknownSession),
        "authentication" => Ok(TurnFailureKind::Authentication),
        "agent_loop" => Ok(TurnFailureKind::AgentLoop),
        "configuration" => Ok(TurnFailureKind::Configuration),
        "persistence" => Ok(TurnFailureKind::Persistence),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn model_position_str(position: ModelExecutionPosition) -> &'static str {
    match position {
        ModelExecutionPosition::ModelStarted => "model_started",
        ModelExecutionPosition::ResponsePersisted => "response_persisted",
        ModelExecutionPosition::ToolsResolved => "tools_resolved",
    }
}

fn decode_model_position(value: &str) -> rusqlite::Result<ModelExecutionPosition> {
    match value {
        "model_started" => Ok(ModelExecutionPosition::ModelStarted),
        "response_persisted" => Ok(ModelExecutionPosition::ResponsePersisted),
        "tools_resolved" => Ok(ModelExecutionPosition::ToolsResolved),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn model_recovery_decision_str(decision: ModelRecoveryDecision) -> &'static str {
    match decision {
        ModelRecoveryDecision::ManualReconciliation => "manual_reconciliation",
        ModelRecoveryDecision::RecoverTools => "recover_tools",
        ModelRecoveryDecision::CompleteTurn => "complete_turn",
        ModelRecoveryDecision::ContinueTurn => "continue_turn",
    }
}

fn decode_model_recovery_decision(value: &str) -> rusqlite::Result<ModelRecoveryDecision> {
    match value {
        "manual_reconciliation" => Ok(ModelRecoveryDecision::ManualReconciliation),
        "recover_tools" => Ok(ModelRecoveryDecision::RecoverTools),
        "complete_turn" => Ok(ModelRecoveryDecision::CompleteTurn),
        "continue_turn" => Ok(ModelRecoveryDecision::ContinueTurn),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn model_recovery_reason_str(reason: ModelRecoveryReason) -> &'static str {
    match reason {
        ModelRecoveryReason::ProviderOutcomeUnknown => "provider_outcome_unknown",
        ModelRecoveryReason::DurableToolResponse => "durable_tool_response",
        ModelRecoveryReason::DurableTerminalResponse => "durable_terminal_response",
        ModelRecoveryReason::ToolsAlreadyResolved => "tools_already_resolved",
        ModelRecoveryReason::IncompleteDurableFacts => "incomplete_durable_facts",
    }
}

fn decode_model_recovery_reason(value: &str) -> rusqlite::Result<ModelRecoveryReason> {
    match value {
        "provider_outcome_unknown" => Ok(ModelRecoveryReason::ProviderOutcomeUnknown),
        "durable_tool_response" => Ok(ModelRecoveryReason::DurableToolResponse),
        "durable_terminal_response" => Ok(ModelRecoveryReason::DurableTerminalResponse),
        "tools_already_resolved" => Ok(ModelRecoveryReason::ToolsAlreadyResolved),
        "incomplete_durable_facts" => Ok(ModelRecoveryReason::IncompleteDurableFacts),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn tool_call_failure_kind_str(kind: ToolCallFailureKind) -> &'static str {
    match kind {
        ToolCallFailureKind::FilesystemBoundaryPolicyViolation => {
            "filesystem_boundary_policy_violation"
        }
    }
}

fn decode_tool_call_row(
    row: &rusqlite::Row<'_>,
    session_id: SessionId,
    turn_id: String,
    offset: usize,
) -> rusqlite::Result<ToolCallSnapshot> {
    let failure_kind = row
        .get::<_, Option<String>>(offset + 14)?
        .map(|value| decode_tool_call_failure_kind(&value))
        .transpose()?;
    let recovery_decision = row
        .get::<_, Option<String>>(offset + 15)?
        .map(|value| decode_tool_recovery_decision(&value))
        .transpose()?;
    let recovery_reason = row
        .get::<_, Option<String>>(offset + 16)?
        .map(|value| decode_tool_recovery_reason(&value))
        .transpose()?;
    Ok(ToolCallSnapshot {
        session_id,
        turn_id,
        call_id: row.get(offset)?,
        invocation_id: ToolInvocationId::parse(row.get(offset + 1)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(offset + 1, Type::Text, Box::new(error))
        })?,
        tool_name: row.get(offset + 2)?,
        invocation_class: row
            .get::<_, Option<String>>(offset + 3)?
            .map(|value| decode_tool_invocation_class(&value))
            .transpose()?,
        declared_recovery_policy: decode_tool_recovery_policy(&row.get::<_, String>(offset + 4)?)?,
        effective_recovery_policy: decode_tool_recovery_policy(&row.get::<_, String>(offset + 5)?)?,
        capability_revision: row.get(offset + 6)?,
        input_digest: row.get(offset + 7)?,
        position: decode_tool_execution_position(&row.get::<_, String>(offset + 8)?)?,
        ledger_revision: u64::try_from(row.get::<_, i64>(offset + 9)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        started_at: row.get(offset + 10)?,
        updated_at: row.get(offset + 11)?,
        state: decode_tool_call_state(&row.get::<_, String>(offset + 12)?)?,
        ended_at: row.get(offset + 13)?,
        failure_kind,
        recovery_decision,
        recovery_reason,
        operator_action_required: row.get(offset + 17)?,
        recovery_attempts: u32::try_from(row.get::<_, i64>(offset + 18)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        recovery_owner: row.get(offset + 19)?,
        recovery_lease_expires_at: row.get(offset + 20)?,
        first_interrupted_at: row.get(offset + 21)?,
    })
}

const fn recovery_policy_allows(
    declared: ToolRecoveryPolicy,
    effective: ToolRecoveryPolicy,
) -> bool {
    matches!(effective, ToolRecoveryPolicy::NeverReplay) || declared as u8 == effective as u8
}

const fn tool_invocation_class_str(class: ToolInvocationClass) -> &'static str {
    match class {
        ToolInvocationClass::Read => "read",
        ToolInvocationClass::FilesystemMutation => "filesystem_mutation",
        ToolInvocationClass::Terminal => "terminal",
        ToolInvocationClass::Browser => "browser",
        ToolInvocationClass::HostControl => "host_control",
        ToolInvocationClass::ArbitraryMcp => "arbitrary_mcp",
        ToolInvocationClass::MemoryCandidate => "memory_candidate",
        ToolInvocationClass::Control => "control",
        ToolInvocationClass::Extension => "extension",
    }
}

fn decode_tool_invocation_class(value: &str) -> rusqlite::Result<ToolInvocationClass> {
    match value {
        "read" => Ok(ToolInvocationClass::Read),
        "filesystem_mutation" => Ok(ToolInvocationClass::FilesystemMutation),
        "terminal" => Ok(ToolInvocationClass::Terminal),
        "browser" => Ok(ToolInvocationClass::Browser),
        "host_control" => Ok(ToolInvocationClass::HostControl),
        "arbitrary_mcp" => Ok(ToolInvocationClass::ArbitraryMcp),
        "memory_candidate" => Ok(ToolInvocationClass::MemoryCandidate),
        "control" => Ok(ToolInvocationClass::Control),
        "extension" => Ok(ToolInvocationClass::Extension),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

const fn tool_recovery_policy_str(policy: ToolRecoveryPolicy) -> &'static str {
    match policy {
        ToolRecoveryPolicy::NeverReplay => "never_replay",
        ToolRecoveryPolicy::RetryWithSameInvocation => "retry_with_same_invocation",
        ToolRecoveryPolicy::ReconcileBeforeRetry => "reconcile_before_retry",
    }
}

fn decode_tool_recovery_policy(value: &str) -> rusqlite::Result<ToolRecoveryPolicy> {
    match value {
        "never_replay" => Ok(ToolRecoveryPolicy::NeverReplay),
        "retry_with_same_invocation" => Ok(ToolRecoveryPolicy::RetryWithSameInvocation),
        "reconcile_before_retry" => Ok(ToolRecoveryPolicy::ReconcileBeforeRetry),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

const fn tool_recovery_decision_str(decision: ToolRecoveryDecision) -> &'static str {
    match decision {
        ToolRecoveryDecision::ResumeAuthorization => "resume_authorization",
        ToolRecoveryDecision::StartEffect => "start_effect",
        ToolRecoveryDecision::RetrySameInvocation => "retry_same_invocation",
        ToolRecoveryDecision::Reconcile => "reconcile",
        ToolRecoveryDecision::RecoverResult => "recover_result",
        ToolRecoveryDecision::ContinueTurn => "continue_turn",
        ToolRecoveryDecision::ManualReconciliation => "manual_reconciliation",
    }
}

fn decode_tool_recovery_decision(value: &str) -> rusqlite::Result<ToolRecoveryDecision> {
    match value {
        "resume_authorization" => Ok(ToolRecoveryDecision::ResumeAuthorization),
        "start_effect" => Ok(ToolRecoveryDecision::StartEffect),
        "retry_same_invocation" => Ok(ToolRecoveryDecision::RetrySameInvocation),
        "reconcile" => Ok(ToolRecoveryDecision::Reconcile),
        "recover_result" => Ok(ToolRecoveryDecision::RecoverResult),
        "continue_turn" => Ok(ToolRecoveryDecision::ContinueTurn),
        "manual_reconciliation" => Ok(ToolRecoveryDecision::ManualReconciliation),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

const fn tool_recovery_reason_str(reason: ToolRecoveryReason) -> &'static str {
    match reason {
        ToolRecoveryReason::EffectNotStarted => "effect_not_started",
        ToolRecoveryReason::SameIdentityReplayAllowed => "same_identity_replay_allowed",
        ToolRecoveryReason::ReconciliationRequired => "reconciliation_required",
        ToolRecoveryReason::ReconciliationConfirmedNoEffect => "reconciliation_confirmed_no_effect",
        ToolRecoveryReason::ReconciliationConfirmedRollback => "reconciliation_confirmed_rollback",
        ToolRecoveryReason::ReconciliationUncertain => "reconciliation_uncertain",
        ToolRecoveryReason::ReplayForbiddenAfterEffectStart => {
            "replay_forbidden_after_effect_start"
        }
        ToolRecoveryReason::EffectAlreadyCommitted => "effect_already_committed",
        ToolRecoveryReason::ResultAlreadyPersisted => "result_already_persisted",
    }
}

fn decode_tool_recovery_reason(value: &str) -> rusqlite::Result<ToolRecoveryReason> {
    match value {
        "effect_not_started" => Ok(ToolRecoveryReason::EffectNotStarted),
        "same_identity_replay_allowed" => Ok(ToolRecoveryReason::SameIdentityReplayAllowed),
        "reconciliation_required" => Ok(ToolRecoveryReason::ReconciliationRequired),
        "reconciliation_confirmed_no_effect" => {
            Ok(ToolRecoveryReason::ReconciliationConfirmedNoEffect)
        }
        "reconciliation_confirmed_rollback" => {
            Ok(ToolRecoveryReason::ReconciliationConfirmedRollback)
        }
        "reconciliation_uncertain" => Ok(ToolRecoveryReason::ReconciliationUncertain),
        "replay_forbidden_after_effect_start" => {
            Ok(ToolRecoveryReason::ReplayForbiddenAfterEffectStart)
        }
        "effect_already_committed" => Ok(ToolRecoveryReason::EffectAlreadyCommitted),
        "result_already_persisted" => Ok(ToolRecoveryReason::ResultAlreadyPersisted),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

const fn tool_execution_position_str(position: ToolExecutionPosition) -> &'static str {
    match position {
        ToolExecutionPosition::Prepared => "prepared",
        ToolExecutionPosition::Authorized => "authorized",
        ToolExecutionPosition::EffectStarted => "effect_started",
        ToolExecutionPosition::EffectCommitted => "effect_committed",
        ToolExecutionPosition::ResultPersisted => "result_persisted",
    }
}

fn decode_tool_execution_position(value: &str) -> rusqlite::Result<ToolExecutionPosition> {
    match value {
        "prepared" => Ok(ToolExecutionPosition::Prepared),
        "authorized" => Ok(ToolExecutionPosition::Authorized),
        "effect_started" => Ok(ToolExecutionPosition::EffectStarted),
        "effect_committed" => Ok(ToolExecutionPosition::EffectCommitted),
        "result_persisted" => Ok(ToolExecutionPosition::ResultPersisted),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn decode_tool_call_state(value: &str) -> rusqlite::Result<ToolCallState> {
    match value {
        "running" => Ok(ToolCallState::Running),
        "succeeded" => Ok(ToolCallState::Succeeded),
        "failed" => Ok(ToolCallState::Failed),
        "rejected" => Ok(ToolCallState::Rejected),
        "abandoned" => Ok(ToolCallState::Abandoned),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn decode_tool_call_failure_kind(value: &str) -> rusqlite::Result<ToolCallFailureKind> {
    match value {
        "filesystem_boundary_policy_violation" => {
            Ok(ToolCallFailureKind::FilesystemBoundaryPolicyViolation)
        }
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn parse_priority(s: &str) -> rusqlite::Result<sylvander_api::session_context::Priority> {
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
#[path = "../../../tests/unit/session_store_sqlite.rs"]
mod tests;
