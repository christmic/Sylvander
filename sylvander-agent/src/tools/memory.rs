//! Memory store abstraction — pluggable backends for long-term agent memory.
//!
//! # M-B design: structured memories
//!
//! A memory entry is more than a string. It carries:
//! - **kind**: what type of fact this is (preference, decision, project fact, ...)
//! - **references**: links to files / sessions / messages / other memories
//! - **importance**: low/medium/high/critical — drives recall priority
//! - **owner identity**: which user/agent/session wrote it
//!
//! Every operation takes a runtime-derived [`MemoryExecutionContext`]. A
//! relationship-memory caller supplies only [`MemoryAppend`]; ownership,
//! identity, timestamps, and provenance remain store-controlled.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sylvander_protocol::SessionContext;
use sylvander_protocol::types::{AgentId, SessionId, UserId};

pub const MAX_MEMORY_CONTENT_BYTES: usize = 16 * 1024;
pub const MAX_MEMORY_QUERY_BYTES: usize = 4 * 1024;
pub const MAX_MEMORY_TAGS: usize = 32;
pub const MAX_MEMORY_TAG_BYTES: usize = 64;
pub const MAX_MEMORY_REFERENCES: usize = 32;
pub const MAX_MEMORY_RESULTS: usize = 50;
pub const MAX_MEMORY_METADATA_ENTRIES: usize = 32;
pub const MAX_MEMORY_METADATA_KEY_BYTES: usize = 64;
pub const MAX_MEMORY_METADATA_VALUE_BYTES: usize = 1024;
pub const MAX_MEMORY_TTL_SECONDS: u64 = 5 * 365 * 24 * 60 * 60;
pub const MAX_RETENTION_GRACE_DAYS: u32 = 365;
pub const MAX_RETENTION_SUPERSEDED_DAYS: u32 = 5 * 365;
pub const MAX_RETENTION_BATCH_LIMIT: u32 = 1_000;
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

const RESERVED_MEMORY_METADATA_KEYS: &[&str] = &[
    "access_count",
    "actor",
    "agent_id",
    "created_at",
    "expires_at",
    "id",
    "last_accessed",
    "owner",
    "provenance",
    "revision",
    "scope",
    "session_id",
    "supersedes",
    "trace_id",
    "updated_at",
    "user_id",
];

const MEMORY_TRACE_DIGEST_DOMAIN: &[u8] = b"sylvander.memory.trace.v1\0";

fn memory_trace_digest(trace_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(MEMORY_TRACE_DIGEST_DOMAIN);
    hasher.update(trace_id.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

/// Stable ownership domain for a memory record. Session state and user
/// profiles are intentionally stored by their own services.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Relationship,
    AgentPrivateCandidate,
    AgentCanonical,
    WorkspaceKnowledge,
}

/// Owner shape determines the scope; invalid scope/owner combinations cannot
/// be represented.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum MemoryOwner {
    Relationship { user_id: UserId, agent_id: AgentId },
    AgentPrivateCandidate { agent_id: AgentId },
    AgentCanonical { agent_id: AgentId },
    WorkspaceKnowledge { workspace_id: String },
}

impl MemoryOwner {
    #[must_use]
    pub const fn scope(&self) -> MemoryScope {
        match self {
            Self::Relationship { .. } => MemoryScope::Relationship,
            Self::AgentPrivateCandidate { .. } => MemoryScope::AgentPrivateCandidate,
            Self::AgentCanonical { .. } => MemoryScope::AgentCanonical,
            Self::WorkspaceKnowledge { .. } => MemoryScope::WorkspaceKnowledge,
        }
    }
}

/// Runtime actor class. Guardian is deliberately distinct from a generic
/// system task so future governed mutations cannot inherit ambient authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryActorKind {
    Worker,
    Guardian,
    SystemService,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryAuthority {
    Untrusted,
    ApplicationIssued,
}

/// Identity snapshot derived by the Agent application for one memory
/// operation. Ordinary Rust callers can hold and forward this opaque value,
/// but cannot mint authority from a caller-created [`SessionContext`].
///
/// Ordinary callers cannot issue application memory authority:
///
/// ```compile_fail
/// use sylvander_agent::tools::MemoryExecutionContext;
/// use sylvander_protocol::SessionContext;
/// let session = SessionContext::new("forged-user", "agent", "session");
/// let _ = MemoryExecutionContext::application_worker(&session);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryExecutionContext {
    authority: MemoryAuthority,
    actor: MemoryActorKind,
    user_id: Option<UserId>,
    agent_id: Option<AgentId>,
    session_id: Option<SessionId>,
    authorized_workspace_ids: Vec<String>,
    trace_id: Option<String>,
}

impl MemoryExecutionContext {
    /// Create an application-issued Worker context.
    ///
    /// This constructor is crate-private so ordinary tools and plugins cannot
    /// turn a caller-created [`SessionContext`] into trusted provenance.
    #[must_use]
    pub(crate) fn application_worker(session: &SessionContext) -> Self {
        Self {
            authority: MemoryAuthority::ApplicationIssued,
            actor: MemoryActorKind::Worker,
            user_id: Some(session.identity.user_id.clone()),
            agent_id: Some(session.identity.agent_id.clone()),
            session_id: Some(session.identity.session_id.clone()),
            authorized_workspace_ids: Vec::new(),
            // Trace identifiers cross a persistence boundary below this type.
            // Keep correlation without retaining caller-controlled text.
            trace_id: session.request.trace_id.as_deref().map(memory_trace_digest),
        }
    }

    #[must_use]
    pub(crate) fn untrusted(session: &SessionContext) -> Self {
        Self {
            authority: MemoryAuthority::Untrusted,
            actor: MemoryActorKind::Worker,
            user_id: Some(session.identity.user_id.clone()),
            agent_id: Some(session.identity.agent_id.clone()),
            session_id: Some(session.identity.session_id.clone()),
            authorized_workspace_ids: Vec::new(),
            // Trace identifiers cross a persistence boundary below this type.
            // Keep correlation while ensuring provenance and audit records can
            // never retain caller-controlled text, controls, or unbounded data.
            trace_id: session.request.trace_id.as_deref().map(memory_trace_digest),
        }
    }

    pub fn relationship_owner(&self) -> Result<MemoryOwner, MemoryStoreError> {
        if self.authority != MemoryAuthority::ApplicationIssued
            || self.actor != MemoryActorKind::Worker
        {
            return Err(MemoryStoreError::AccessDenied);
        }
        let (Some(user_id), Some(agent_id), Some(session_id)) =
            (&self.user_id, &self.agent_id, &self.session_id)
        else {
            return Err(MemoryStoreError::AccessDenied);
        };
        if user_id.0.is_empty() || agent_id.0.is_empty() || session_id.0.is_empty() {
            return Err(MemoryStoreError::AccessDenied);
        }
        Ok(MemoryOwner::Relationship {
            user_id: user_id.clone(),
            agent_id: agent_id.clone(),
        })
    }

    #[must_use]
    pub const fn actor(&self) -> MemoryActorKind {
        self.actor
    }

    #[must_use]
    pub fn user_id(&self) -> Option<&UserId> {
        self.user_id.as_ref()
    }

    #[must_use]
    pub fn agent_id(&self) -> Option<&AgentId> {
        self.agent_id.as_ref()
    }

    #[must_use]
    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    #[must_use]
    pub fn authorized_workspace_ids(&self) -> &[String] {
        &self.authorized_workspace_ids
    }

    #[must_use]
    pub fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_deref()
    }

    pub(super) fn provenance(&self) -> MemoryProvenance {
        debug_assert_eq!(self.authority, MemoryAuthority::ApplicationIssued);
        MemoryProvenance {
            actor: self.actor,
            user_id: self.user_id.clone(),
            agent_id: self.agent_id.clone(),
            session_id: self.session_id.clone(),
            trace_id: self.trace_id.clone(),
            source: MemoryProvenanceSource::Runtime,
            trusted: true,
        }
    }

    #[cfg(test)]
    pub(super) fn privileged_for_test(actor: MemoryActorKind) -> Self {
        Self {
            authority: MemoryAuthority::ApplicationIssued,
            actor,
            user_id: Some(UserId::new("alice")),
            agent_id: Some(AgentId::new("agent-a")),
            session_id: Some(SessionId::new("session")),
            authorized_workspace_ids: Vec::new(),
            trace_id: None,
        }
    }
}

/// Caller-controlled fields for a new relationship memory. Store-controlled
/// identity, ownership, timestamps, counters, and provenance are deliberately
/// absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAppend {
    pub kind: MemoryKind,
    pub content: String,
    pub references: Vec<MemoryReference>,
    pub tags: Vec<String>,
    pub importance: Importance,
    pub metadata: HashMap<String, String>,
    /// Relative expiry requested by the caller. The store validates this and
    /// derives the absolute timestamp from its own clock.
    pub expires_after_secs: Option<u64>,
}

impl MemoryAppend {
    #[must_use]
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            kind: MemoryKind::AgentNote,
            content: content.into(),
            references: Vec::new(),
            tags: Vec::new(),
            importance: Importance::Medium,
            metadata: HashMap::new(),
            expires_after_secs: None,
        }
    }

    #[must_use]
    pub fn with_kind(mut self, kind: MemoryKind) -> Self {
        self.kind = kind;
        self
    }

    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    #[must_use]
    pub fn with_reference(mut self, reference: MemoryReference) -> Self {
        self.references.push(reference);
        self
    }

    #[must_use]
    pub fn with_importance(mut self, importance: Importance) -> Self {
        self.importance = importance;
        self
    }

    #[must_use]
    pub fn with_ttl(mut self, seconds: u64) -> Self {
        self.expires_after_secs = Some(seconds);
        self
    }
}

/// Caller-controlled changes to an active relationship memory. Omitted fields
/// remain unchanged; ownership and provenance are never patchable.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MemoryPatch {
    pub kind: Option<MemoryKind>,
    pub content: Option<String>,
    pub references: Option<Vec<MemoryReference>>,
    pub tags: Option<Vec<String>>,
    pub importance: Option<Importance>,
    pub metadata: Option<HashMap<String, String>>,
    pub expiry: Option<MemoryExpiryPatch>,
}

/// Explicit expiry mutation; absence from [`MemoryPatch`] means preserve it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryExpiryPatch {
    Never,
    AfterSeconds(u64),
}

/// Validated relationship-memory lifecycle policy. Its revision identifies
/// the exact policy used to derive each entry's effective expiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipMemoryRetentionPolicy {
    revision: u64,
    default_ttl_days: u32,
    max_ttl_days: u32,
    expiry_grace_days: u32,
    superseded_retention_days: u32,
    batch_limit: u32,
}

impl RelationshipMemoryRetentionPolicy {
    pub fn new(
        revision: u64,
        default_ttl_days: u32,
        max_ttl_days: u32,
        expiry_grace_days: u32,
        superseded_retention_days: u32,
        batch_limit: u32,
    ) -> Result<Self, MemoryStoreError> {
        if revision == 0
            || i64::try_from(revision).is_err()
            || default_ttl_days == 0
            || default_ttl_days > max_ttl_days
            || u64::from(max_ttl_days) * SECONDS_PER_DAY > MAX_MEMORY_TTL_SECONDS
            || expiry_grace_days > MAX_RETENTION_GRACE_DAYS
            || superseded_retention_days > MAX_RETENTION_SUPERSEDED_DAYS
            || batch_limit == 0
            || batch_limit > MAX_RETENTION_BATCH_LIMIT
        {
            return Err(MemoryStoreError::InvalidInput);
        }
        Ok(Self {
            revision,
            default_ttl_days,
            max_ttl_days,
            expiry_grace_days,
            superseded_retention_days,
            batch_limit,
        })
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    #[must_use]
    pub const fn default_ttl_days(&self) -> u32 {
        self.default_ttl_days
    }
    #[must_use]
    pub const fn max_ttl_days(&self) -> u32 {
        self.max_ttl_days
    }
    #[must_use]
    pub const fn expiry_grace_days(&self) -> u32 {
        self.expiry_grace_days
    }
    #[must_use]
    pub const fn superseded_retention_days(&self) -> u32 {
        self.superseded_retention_days
    }
    #[must_use]
    pub const fn batch_limit(&self) -> u32 {
        self.batch_limit
    }

    pub(super) fn apply_append(
        &self,
        mut append: MemoryAppend,
    ) -> Result<MemoryAppend, MemoryStoreError> {
        let requested = append
            .expires_after_secs
            .unwrap_or(u64::from(self.default_ttl_days) * SECONDS_PER_DAY);
        if requested == 0 || requested > u64::from(self.max_ttl_days) * SECONDS_PER_DAY {
            return Err(MemoryStoreError::InvalidInput);
        }
        append.expires_after_secs = Some(requested);
        Ok(append)
    }

    pub(super) fn validate_patch(&self, patch: &MemoryPatch) -> Result<(), MemoryStoreError> {
        match patch.expiry {
            Some(MemoryExpiryPatch::Never) => Err(MemoryStoreError::InvalidInput),
            Some(MemoryExpiryPatch::AfterSeconds(seconds))
                if seconds > u64::from(self.max_ttl_days) * SECONDS_PER_DAY =>
            {
                Err(MemoryStoreError::InvalidInput)
            }
            _ => Ok(()),
        }
    }
}

impl Default for RelationshipMemoryRetentionPolicy {
    fn default() -> Self {
        Self::new(1, 180, 365, 7, 30, 100).expect("default retention policy must be valid")
    }
}

/// Immutable origin recorded when a memory is created. These fields are
/// derived from the runtime execution context, never from [`MemoryAppend`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProvenance {
    pub actor: MemoryActorKind,
    pub user_id: Option<UserId>,
    pub agent_id: Option<AgentId>,
    pub session_id: Option<SessionId>,
    pub trace_id: Option<String>,
    pub source: MemoryProvenanceSource,
    pub trusted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryProvenanceSource {
    Runtime,
}

// ---------------------------------------------------------------------------
// MemoryEntry
// ---------------------------------------------------------------------------

/// A single entry in an agent's long-term memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Unique identifier for this entry.
    pub id: String,

    /// Who created this memory. The store enforces per-identity
    /// visibility: a user can only see their own memories (and
    /// memories explicitly shared into their session by other agents).
    pub owner: MemoryOwner,

    /// What kind of memory this is. Use this to filter "show me
    /// preferences only" or "show me all decisions".
    pub kind: MemoryKind,

    /// The memory content (free-form text).
    pub content: String,

    /// Structured cross-references — to files this memory concerns,
    /// to sessions where it was discussed, to other memories that
    /// relate, etc.
    #[serde(default)]
    pub references: Vec<MemoryReference>,

    /// Free-form tags for ad-hoc categorization.
    #[serde(default)]
    pub tags: Vec<String>,

    /// How important this memory is. Drives recall priority and
    /// helps decide which entries survive a future compaction pass.
    #[serde(default)]
    pub importance: Importance,

    /// Unix timestamp when this entry was created.
    pub created_at: i64,

    /// Last access timestamp (M-B Phase 2: for decay / popularity
    /// ranking). `None` until first read.
    #[serde(default)]
    pub last_accessed: Option<i64>,

    /// Number of times recalled.
    #[serde(default)]
    pub access_count: u32,

    /// Free-form key/value metadata (legacy / extension).
    #[serde(default)]
    pub metadata: HashMap<String, String>,

    /// Monotonic optimistic-concurrency revision, starting at one.
    pub revision: u64,

    /// Store-controlled last mutation timestamp.
    pub updated_at: i64,

    /// Store-derived absolute expiry. Expired records are not visible to
    /// ordinary relationship reads.
    pub expires_at: Option<i64>,

    /// Public id of a replacement memory, once superseded.
    pub superseded_by: Option<String>,

    /// Immutable runtime-derived creation provenance.
    pub provenance: MemoryProvenance,

    /// Policy revision used to derive this entry's effective expiry.
    pub retention_policy_revision: u64,
}

impl MemoryEntry {
    pub(super) fn materialize(
        id: String,
        owner: MemoryOwner,
        append: MemoryAppend,
        provenance: MemoryProvenance,
        retention_policy_revision: u64,
        now: i64,
    ) -> Result<Self, MemoryStoreError> {
        let expires_at = match append.expires_after_secs {
            Some(ttl) => Some(
                now.checked_add(i64::try_from(ttl).map_err(|_| MemoryStoreError::InvalidInput)?)
                    .ok_or(MemoryStoreError::InvalidInput)?,
            ),
            None => None,
        };
        Ok(Self {
            id,
            owner,
            kind: append.kind,
            content: append.content,
            references: append.references,
            tags: append.tags,
            importance: append.importance,
            created_at: now,
            last_accessed: None,
            access_count: 0,
            metadata: append.metadata,
            revision: 1,
            updated_at: now,
            expires_at,
            superseded_by: None,
            provenance,
            retention_policy_revision,
        })
    }
}

// ---------------------------------------------------------------------------
// MemoryKind
// ---------------------------------------------------------------------------

/// What kind of fact this memory represents.
///
/// Use this to filter recall: `search(kind=Preference)` returns only
/// the user's preferences. New kinds are non-breaking — older code
/// just sees an `Unknown` variant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// A user preference ("User prefers dark mode").
    Preference,
    /// A project fact ("This project uses Rust + tokio").
    ProjectFact,
    /// A decision made ("We chose X over Y because ...").
    Decision,
    /// Pointer to a past conversation for follow-up ("User asked
    /// about X on 2026-07-01, see session s-123").
    ConversationRef { session_id: SessionId },
    /// Free-form note from the agent.
    AgentNote,
    /// Catch-all for forward compatibility — newer code may add
    /// kinds the local implementation doesn't recognize.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// MemoryReference
// ---------------------------------------------------------------------------

/// A structured link from a memory to something concrete.
///
/// Recall tools can resolve these ("show me the memory about X,
/// and also show me the file it refers to").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum MemoryReference {
    /// A file path (resolved relative to the session's `fs_root`).
    File { path: PathBuf },
    /// Another session.
    Session { session_id: SessionId },
    /// A specific message in a session.
    Message { session_id: SessionId, seq: u32 },
    /// An external URL (read-only).
    Url { url: String },
    /// Another memory entry (graph of related memories).
    Memory { memory_id: String },
}

// ---------------------------------------------------------------------------
// Importance
// ---------------------------------------------------------------------------

/// How important this memory is. Drives recall priority and
/// future compaction policy.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Importance {
    Low,
    #[default]
    Medium,
    High,
    Critical,
}

// ---------------------------------------------------------------------------
// MemoryStore trait
// ---------------------------------------------------------------------------

/// Backend for long-term agent memory.
///
/// All operations are scoped to the calling identity. The store
/// MUST refuse to surface a memory whose `owner` does not match
/// the calling `ctx` (or has been explicitly shared).
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Append a relationship entry whose owner is derived from `ctx`.
    async fn append_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        append: MemoryAppend,
    ) -> Result<MemoryEntry, MemoryStoreError>;

    /// Search relationship entries owned by the calling identity.
    async fn search_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        query: &str,
        filter: MemoryFilter,
    ) -> Result<Vec<MemoryEntry>, MemoryStoreError>;

    /// Patch an active entry using optimistic concurrency.
    async fn update_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
        expected_revision: u64,
        patch: MemoryPatch,
    ) -> Result<MemoryEntry, MemoryStoreError>;

    /// Atomically replace an active entry and retire the previous revision.
    async fn supersede_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
        expected_revision: u64,
        replacement: MemoryAppend,
    ) -> Result<MemoryEntry, MemoryStoreError>;

    /// Delete a relationship entry without revealing another owner's entry.
    async fn delete_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
        expected_revision: u64,
    ) -> Result<(), MemoryStoreError>;

    /// Get one relationship entry without revealing another owner's entry.
    async fn get_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
    ) -> Result<Option<MemoryEntry>, MemoryStoreError>;
}

/// Optional narrowing filter for search.
#[derive(Debug, Default, Clone)]
pub struct MemoryFilter {
    pub kind: Option<MemoryKind>,
    pub min_importance: Option<Importance>,
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// MemoryStoreError
// ---------------------------------------------------------------------------

/// Errors from memory store operations.
#[derive(Debug, thiserror::Error)]
pub enum MemoryStoreError {
    /// Input is malformed or exceeds a public boundary.
    #[error("memory input is invalid")]
    InvalidInput,
    /// A store operation failed.
    #[error("store error: {0}")]
    Store(String),
    /// A search operation failed.
    #[error("search error: {0}")]
    Search(String),
    /// A delete operation failed.
    #[error("delete error: {0}")]
    Delete(String),
    /// Entry exists but the caller is not allowed to see it.
    #[error("memory access denied")]
    AccessDenied,
    /// The visible record changed since the caller read it.
    #[error("memory revision conflict")]
    Conflict,
    /// Missing and foreign-owner records deliberately share one result.
    #[error("memory not found")]
    NotFound,
}

// ---------------------------------------------------------------------------
// InMemoryMemoryStore
// ---------------------------------------------------------------------------

/// An in-memory memory store backed by a `Vec<MemoryEntry>`. Enforces
/// per-identity visibility: `search` and `get` only return entries whose typed
/// relationship owner matches the caller's runtime identity.
///
/// Search is case-insensitive substring matching on `content`. M-B
/// Phase 2 will replace this with FTS5 / embedding search.
#[derive(Debug)]
pub struct InMemoryMemoryStore {
    entries: tokio::sync::RwLock<Vec<MemoryEntry>>,
    retention_policy: RelationshipMemoryRetentionPolicy,
}

impl Default for InMemoryMemoryStore {
    fn default() -> Self {
        Self {
            entries: tokio::sync::RwLock::new(Vec::new()),
            retention_policy: RelationshipMemoryRetentionPolicy::default(),
        }
    }
}

impl InMemoryMemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_retention_policy(policy: RelationshipMemoryRetentionPolicy) -> Self {
        Self {
            entries: tokio::sync::RwLock::new(Vec::new()),
            retention_policy: policy,
        }
    }
}

#[async_trait]
impl MemoryStore for InMemoryMemoryStore {
    async fn append_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        append: MemoryAppend,
    ) -> Result<MemoryEntry, MemoryStoreError> {
        let owner = ctx.relationship_owner()?;
        let append = self.retention_policy.apply_append(append)?;
        validate_append(&append)?;
        let entry = MemoryEntry::materialize(
            uuid::Uuid::new_v4().to_string(),
            owner,
            append,
            ctx.provenance(),
            self.retention_policy.revision(),
            crate::session::now_secs(),
        )?;
        self.entries.write().await.push(entry.clone());
        Ok(entry)
    }

    async fn search_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        query: &str,
        filter: MemoryFilter,
    ) -> Result<Vec<MemoryEntry>, MemoryStoreError> {
        let owner = ctx.relationship_owner()?;
        if query.len() > MAX_MEMORY_QUERY_BYTES
            || filter
                .limit
                .is_some_and(|limit| limit == 0 || limit > MAX_MEMORY_RESULTS)
        {
            return Err(MemoryStoreError::InvalidInput);
        }
        let query_lower = query.to_lowercase();
        let now = crate::session::now_secs();
        let entries = self.entries.read().await;
        let mut results: Vec<MemoryEntry> = entries
            .iter()
            .filter(|entry| entry.owner == owner)
            .filter(|entry| is_active(entry, now))
            .filter(|e| query.is_empty() || e.content.to_lowercase().contains(&query_lower))
            .filter(|e| filter.kind.as_ref().is_none_or(|k| &e.kind == k))
            .filter(|e| filter.min_importance.is_none_or(|i| e.importance >= i))
            .cloned()
            .collect();
        if let Some(limit) = filter.limit {
            results.truncate(limit);
        }
        Ok(results)
    }

    async fn update_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
        expected_revision: u64,
        patch: MemoryPatch,
    ) -> Result<MemoryEntry, MemoryStoreError> {
        let owner = ctx.relationship_owner()?;
        validate_memory_id(id)?;
        validate_patch(&patch)?;
        self.retention_policy.validate_patch(&patch)?;
        let updates_expiry = patch.expiry.is_some();
        validate_revision(expected_revision)?;
        let now = crate::session::now_secs();
        let mut entries = self.entries.write().await;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.id == id && entry.owner == owner && is_active(entry, now))
            .ok_or_else(memory_not_visible)?;
        if entry.revision != expected_revision {
            return Err(MemoryStoreError::Conflict);
        }
        apply_patch(entry, patch, now)?;
        if updates_expiry {
            entry.retention_policy_revision = self.retention_policy.revision();
        }
        Ok(entry.clone())
    }

    async fn supersede_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
        expected_revision: u64,
        replacement: MemoryAppend,
    ) -> Result<MemoryEntry, MemoryStoreError> {
        let owner = ctx.relationship_owner()?;
        validate_memory_id(id)?;
        let replacement = self.retention_policy.apply_append(replacement)?;
        validate_append(&replacement)?;
        validate_revision(expected_revision)?;
        let now = crate::session::now_secs();
        let replacement = MemoryEntry::materialize(
            uuid::Uuid::new_v4().to_string(),
            owner.clone(),
            replacement,
            ctx.provenance(),
            self.retention_policy.revision(),
            now,
        )?;
        let mut entries = self.entries.write().await;
        let original = entries
            .iter_mut()
            .find(|entry| entry.id == id && entry.owner == owner && is_active(entry, now))
            .ok_or_else(memory_not_visible)?;
        if original.revision != expected_revision {
            return Err(MemoryStoreError::Conflict);
        }
        original.revision = next_revision(original.revision)?;
        original.updated_at = now;
        original.superseded_by = Some(replacement.id.clone());
        entries.push(replacement.clone());
        Ok(replacement)
    }

    async fn delete_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
        expected_revision: u64,
    ) -> Result<(), MemoryStoreError> {
        let owner = ctx.relationship_owner()?;
        validate_memory_id(id)?;
        validate_revision(expected_revision)?;
        let now = crate::session::now_secs();
        let mut entries = self.entries.write().await;
        let Some(index) = entries
            .iter()
            .position(|entry| entry.id == id && entry.owner == owner && is_active(entry, now))
        else {
            return Err(memory_not_visible());
        };
        if entries[index].revision != expected_revision {
            return Err(MemoryStoreError::Conflict);
        }
        if entries
            .iter()
            .any(|entry| entry.superseded_by.as_deref() == Some(id))
        {
            return Err(MemoryStoreError::Conflict);
        }
        entries.remove(index);
        Ok(())
    }

    async fn get_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
    ) -> Result<Option<MemoryEntry>, MemoryStoreError> {
        let owner = ctx.relationship_owner()?;
        validate_memory_id(id)?;
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|entry| {
                entry.id == id
                    && entry.owner == owner
                    && is_active(entry, crate::session::now_secs())
            })
            .cloned())
    }
}

pub(super) fn validate_append(append: &MemoryAppend) -> Result<(), MemoryStoreError> {
    if append.content.is_empty()
        || append.content.len() > MAX_MEMORY_CONTENT_BYTES
        || append.tags.len() > MAX_MEMORY_TAGS
        || append.references.len() > MAX_MEMORY_REFERENCES
        || append.metadata.len() > MAX_MEMORY_METADATA_ENTRIES
        || append
            .expires_after_secs
            .is_some_and(|ttl| ttl == 0 || ttl > MAX_MEMORY_TTL_SECONDS)
        || append
            .tags
            .iter()
            .any(|tag| tag.is_empty() || tag.len() > MAX_MEMORY_TAG_BYTES)
        || append.metadata.iter().any(|(key, value)| {
            let normalized = key.to_ascii_lowercase();
            key.is_empty()
                || key.len() > MAX_MEMORY_METADATA_KEY_BYTES
                || value.len() > MAX_MEMORY_METADATA_VALUE_BYTES
                || normalized.starts_with("sylvander.")
                || RESERVED_MEMORY_METADATA_KEYS.contains(&normalized.as_str())
        })
    {
        return Err(MemoryStoreError::InvalidInput);
    }
    Ok(())
}

pub(super) fn validate_patch(patch: &MemoryPatch) -> Result<(), MemoryStoreError> {
    if patch == &MemoryPatch::default() {
        return Err(MemoryStoreError::InvalidInput);
    }
    let candidate = MemoryAppend {
        kind: patch.kind.clone().unwrap_or(MemoryKind::AgentNote),
        content: patch.content.clone().unwrap_or_else(|| "valid".into()),
        references: patch.references.clone().unwrap_or_default(),
        tags: patch.tags.clone().unwrap_or_default(),
        importance: patch.importance.unwrap_or_default(),
        metadata: patch.metadata.clone().unwrap_or_default(),
        expires_after_secs: match patch.expiry {
            Some(MemoryExpiryPatch::AfterSeconds(seconds)) => Some(seconds),
            _ => None,
        },
    };
    validate_append(&candidate)
}

pub(super) fn validate_revision(revision: u64) -> Result<(), MemoryStoreError> {
    if revision == 0 || i64::try_from(revision).is_err() {
        return Err(MemoryStoreError::InvalidInput);
    }
    Ok(())
}

pub(super) fn next_revision(revision: u64) -> Result<u64, MemoryStoreError> {
    revision.checked_add(1).ok_or(MemoryStoreError::Conflict)
}

pub(super) fn apply_patch(
    entry: &mut MemoryEntry,
    patch: MemoryPatch,
    now: i64,
) -> Result<(), MemoryStoreError> {
    if let Some(kind) = patch.kind {
        entry.kind = kind;
    }
    if let Some(content) = patch.content {
        entry.content = content;
    }
    if let Some(references) = patch.references {
        entry.references = references;
    }
    if let Some(tags) = patch.tags {
        entry.tags = tags;
    }
    if let Some(importance) = patch.importance {
        entry.importance = importance;
    }
    if let Some(metadata) = patch.metadata {
        entry.metadata = metadata;
    }
    if let Some(expiry) = patch.expiry {
        entry.expires_at = match expiry {
            MemoryExpiryPatch::Never => None,
            MemoryExpiryPatch::AfterSeconds(seconds) => Some(
                now.checked_add(
                    i64::try_from(seconds).map_err(|_| MemoryStoreError::InvalidInput)?,
                )
                .ok_or(MemoryStoreError::InvalidInput)?,
            ),
        };
    }
    entry.revision = next_revision(entry.revision)?;
    entry.updated_at = now;
    Ok(())
}

pub(super) fn validate_memory_id(id: &str) -> Result<(), MemoryStoreError> {
    if id.is_empty() || id.len() > 128 {
        return Err(MemoryStoreError::InvalidInput);
    }
    Ok(())
}

pub(super) fn memory_not_visible() -> MemoryStoreError {
    MemoryStoreError::NotFound
}

pub(super) fn is_active(entry: &MemoryEntry, now: i64) -> bool {
    entry.superseded_by.is_none() && entry.expires_at.is_none_or(|expiry| expiry > now)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn session(user: &str, agent: &str, session: &str) -> SessionContext {
        SessionContext::new(user, agent, session)
    }

    fn worker(session: &SessionContext) -> MemoryExecutionContext {
        MemoryExecutionContext::application_worker(session)
    }

    fn privileged(actor: MemoryActorKind) -> MemoryExecutionContext {
        MemoryExecutionContext {
            authority: MemoryAuthority::ApplicationIssued,
            actor,
            user_id: Some(UserId::new("alice")),
            agent_id: Some(AgentId::new("a1")),
            session_id: Some(SessionId::new("s1")),
            authorized_workspace_ids: Vec::new(),
            trace_id: None,
        }
    }

    #[tokio::test]
    async fn relationship_append_search_and_filters() {
        let store = InMemoryMemoryStore::new();
        let alice = session("alice", "a1", "s1");
        let ctx = worker(&alice);
        let preference = store
            .append_relationship(
                &ctx,
                MemoryAppend::new("The user prefers Rust")
                    .with_kind(MemoryKind::Preference)
                    .with_tag("language")
                    .with_importance(Importance::High),
            )
            .await
            .unwrap();
        assert_eq!(preference.revision, 1);
        assert_eq!(preference.provenance.actor, MemoryActorKind::Worker);
        assert_eq!(
            preference.provenance.source,
            MemoryProvenanceSource::Runtime
        );
        assert!(preference.provenance.trusted);
        store
            .append_relationship(
                &ctx,
                MemoryAppend::new("we chose tokio").with_kind(MemoryKind::Decision),
            )
            .await
            .unwrap();

        let results = store
            .search_relationship(
                &ctx,
                "RUST",
                MemoryFilter {
                    kind: Some(MemoryKind::Preference),
                    min_importance: Some(Importance::High),
                    limit: Some(1),
                },
            )
            .await
            .unwrap();
        assert_eq!(results, [preference]);
    }

    #[tokio::test]
    async fn relationship_operations_isolate_user_and_agent() {
        let store = InMemoryMemoryStore::new();
        let alice = worker(&session("alice", "a1", "s1"));
        let bob = worker(&session("bob", "a1", "s2"));
        let other_agent = worker(&session("alice", "a2", "s3"));
        let entry = store
            .append_relationship(&alice, MemoryAppend::new("alice secret"))
            .await
            .unwrap();

        for outsider in [&bob, &other_agent] {
            assert!(
                store
                    .search_relationship(outsider, "", MemoryFilter::default())
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                store
                    .get_relationship(outsider, &entry.id)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            store
                .get_relationship(&alice, &entry.id)
                .await
                .unwrap()
                .unwrap()
                .content,
            "alice secret"
        );
    }

    #[tokio::test]
    async fn foreign_and_missing_deletes_are_indistinguishable() {
        let store = InMemoryMemoryStore::new();
        let alice = worker(&session("alice", "a1", "s1"));
        let bob = worker(&session("bob", "a1", "s2"));
        let entry = store
            .append_relationship(&alice, MemoryAppend::new("keep"))
            .await
            .unwrap();

        let foreign = store
            .delete_relationship(&bob, &entry.id, entry.revision)
            .await
            .unwrap_err();
        let missing = store
            .delete_relationship(&bob, "00000000-0000-0000-0000-000000000000", entry.revision)
            .await
            .unwrap_err();
        assert_eq!(foreign.to_string(), missing.to_string());
        assert!(
            store
                .get_relationship(&alice, &entry.id)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn guardian_and_system_fail_closed_for_relationship_operations() {
        let store = InMemoryMemoryStore::new();
        for ctx in [
            privileged(MemoryActorKind::Guardian),
            privileged(MemoryActorKind::SystemService),
        ] {
            assert!(matches!(
                store
                    .append_relationship(&ctx, MemoryAppend::new("forbidden"))
                    .await,
                Err(MemoryStoreError::AccessDenied)
            ));
            assert!(matches!(
                store
                    .search_relationship(&ctx, "", MemoryFilter::default())
                    .await,
                Err(MemoryStoreError::AccessDenied)
            ));
            assert!(matches!(
                store.get_relationship(&ctx, "valid-id").await,
                Err(MemoryStoreError::AccessDenied)
            ));
            assert!(matches!(
                store
                    .update_relationship(&ctx, "valid-id", 1, MemoryPatch::default())
                    .await,
                Err(MemoryStoreError::AccessDenied)
            ));
            assert!(matches!(
                store
                    .supersede_relationship(&ctx, "valid-id", 1, MemoryAppend::new("forbidden"))
                    .await,
                Err(MemoryStoreError::AccessDenied)
            ));
            assert!(matches!(
                store.delete_relationship(&ctx, "valid-id", 1).await,
                Err(MemoryStoreError::AccessDenied)
            ));
        }
    }

    #[tokio::test]
    async fn incomplete_worker_context_fails_closed() {
        let store = InMemoryMemoryStore::new();
        let ctx = MemoryExecutionContext {
            authority: MemoryAuthority::ApplicationIssued,
            actor: MemoryActorKind::Worker,
            user_id: Some(UserId::new("alice")),
            agent_id: Some(AgentId::new("a1")),
            session_id: None,
            authorized_workspace_ids: Vec::new(),
            trace_id: None,
        };
        assert!(matches!(
            store
                .append_relationship(&ctx, MemoryAppend::new("forbidden"))
                .await,
            Err(MemoryStoreError::AccessDenied)
        ));
    }

    #[tokio::test]
    async fn public_memory_bounds_fail_closed() {
        let store = InMemoryMemoryStore::new();
        let ctx = worker(&session("alice", "a1", "s1"));
        assert!(matches!(
            store
                .append_relationship(
                    &ctx,
                    MemoryAppend::new("x".repeat(MAX_MEMORY_CONTENT_BYTES + 1))
                )
                .await,
            Err(MemoryStoreError::InvalidInput)
        ));
        for ttl in [0, MAX_MEMORY_TTL_SECONDS + 1] {
            assert!(matches!(
                store
                    .append_relationship(&ctx, MemoryAppend::new("ttl").with_ttl(ttl))
                    .await,
                Err(MemoryStoreError::InvalidInput)
            ));
        }
        for key in [
            "provenance",
            "owner",
            "scope",
            "revision",
            "actor",
            "user_id",
            "agent_id",
            "session_id",
            "trace_id",
            "SYLVANDER.audit",
        ] {
            let mut append = MemoryAppend::new("forged metadata");
            append.metadata.insert(key.into(), "attacker".into());
            assert!(matches!(
                store.append_relationship(&ctx, append).await,
                Err(MemoryStoreError::InvalidInput)
            ));
        }
        assert!(matches!(
            store
                .search_relationship(
                    &ctx,
                    &"q".repeat(MAX_MEMORY_QUERY_BYTES + 1),
                    MemoryFilter::default()
                )
                .await,
            Err(MemoryStoreError::InvalidInput)
        ));
        assert!(matches!(
            store
                .search_relationship(
                    &ctx,
                    "",
                    MemoryFilter {
                        limit: Some(MAX_MEMORY_RESULTS + 1),
                        ..MemoryFilter::default()
                    }
                )
                .await,
            Err(MemoryStoreError::InvalidInput)
        ));
    }

    #[tokio::test]
    async fn delete_owned_entry() {
        let store = InMemoryMemoryStore::new();
        let ctx = worker(&session("alice", "a1", "s1"));
        let entry = store
            .append_relationship(&ctx, MemoryAppend::new("drop"))
            .await
            .unwrap();
        assert!(matches!(
            store
                .delete_relationship(&ctx, &entry.id, entry.revision + 1)
                .await,
            Err(MemoryStoreError::Conflict)
        ));
        store
            .delete_relationship(&ctx, &entry.id, entry.revision)
            .await
            .unwrap();
        assert!(
            store
                .get_relationship(&ctx, &entry.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn delete_restricts_live_supersession_references() {
        let store = InMemoryMemoryStore::new();
        let ctx = worker(&session("alice", "a1", "s1"));
        let original = store
            .append_relationship(&ctx, MemoryAppend::new("old"))
            .await
            .unwrap();
        let replacement = store
            .supersede_relationship(
                &ctx,
                &original.id,
                original.revision,
                MemoryAppend::new("new"),
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .delete_relationship(&ctx, &replacement.id, replacement.revision)
                .await,
            Err(MemoryStoreError::Conflict)
        ));
    }

    #[tokio::test]
    async fn only_expiry_patch_adopts_current_retention_policy_revision() {
        let policy = RelationshipMemoryRetentionPolicy::new(2, 2, 3, 1, 2, 10).unwrap();
        let store = InMemoryMemoryStore::with_retention_policy(policy);
        let ctx = worker(&session("alice", "a1", "s1"));
        let entry = store
            .append_relationship(&ctx, MemoryAppend::new("before"))
            .await
            .unwrap();
        store.entries.write().await[0].retention_policy_revision = 1;

        let content = store
            .update_relationship(
                &ctx,
                &entry.id,
                entry.revision,
                MemoryPatch {
                    content: Some("after".into()),
                    ..MemoryPatch::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(content.retention_policy_revision, 1);
        let expiry = store
            .update_relationship(
                &ctx,
                &entry.id,
                content.revision,
                MemoryPatch {
                    expiry: Some(MemoryExpiryPatch::AfterSeconds(60)),
                    ..MemoryPatch::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(expiry.retention_policy_revision, 2);
    }

    #[tokio::test]
    async fn update_and_supersede_are_cas_guarded_and_hide_inactive() {
        let store = InMemoryMemoryStore::new();
        let ctx = worker(&session("alice", "a1", "s1"));
        let original = store
            .append_relationship(&ctx, MemoryAppend::new("old").with_ttl(60))
            .await
            .unwrap();
        let patch = MemoryPatch {
            content: Some("updated".into()),
            importance: Some(Importance::Critical),
            expiry: Some(MemoryExpiryPatch::AfterSeconds(30)),
            ..MemoryPatch::default()
        };
        assert!(matches!(
            store
                .update_relationship(&ctx, &original.id, 2, patch.clone())
                .await,
            Err(MemoryStoreError::Conflict)
        ));
        let updated = store
            .update_relationship(&ctx, &original.id, 1, patch)
            .await
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.content, "updated");
        assert_eq!(updated.importance, Importance::Critical);
        assert!(updated.expires_at.is_some());
        assert_eq!(updated.provenance, original.provenance);

        let replacement = store
            .supersede_relationship(
                &ctx,
                &original.id,
                updated.revision,
                MemoryAppend::new("replacement"),
            )
            .await
            .unwrap();
        assert_eq!(replacement.revision, 1);
        assert!(
            store
                .get_relationship(&ctx, &original.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .search_relationship(&ctx, "", MemoryFilter::default())
                .await
                .unwrap(),
            [replacement]
        );
        assert!(matches!(
            store.delete_relationship(&ctx, &original.id, 3).await,
            Err(MemoryStoreError::NotFound)
        ));
    }

    #[test]
    fn append_builders_preserve_caller_fields_only() {
        let append = MemoryAppend::new("we chose Rust")
            .with_kind(MemoryKind::Decision)
            .with_tag("architecture")
            .with_importance(Importance::High)
            .with_reference(MemoryReference::File {
                path: "/Cargo.toml".into(),
            });
        assert_eq!(append.kind, MemoryKind::Decision);
        assert_eq!(append.importance, Importance::High);
        assert_eq!(append.references.len(), 1);
        assert_eq!(append.tags, ["architecture"]);
    }

    #[test]
    fn application_context_hashes_untrusted_trace_identifiers() {
        let raw_trace = format!("private\n\0{}", "x".repeat(128 * 1024));
        let session = session("alice", "a1", "s1").with_trace_id(&raw_trace);
        let worker = MemoryExecutionContext::application_worker(&session);
        assert_eq!(worker.actor(), MemoryActorKind::Worker);
        assert_eq!(worker.user_id(), Some(&UserId::new("alice")));
        assert_eq!(worker.agent_id(), Some(&AgentId::new("a1")));
        assert_eq!(worker.session_id(), Some(&SessionId::new("s1")));
        let trace = worker.trace_id().unwrap();
        assert_eq!(trace, memory_trace_digest(&raw_trace));
        assert_eq!(trace.len(), 71);
        assert!(trace.starts_with("sha256:"));
        assert!(trace[7..].bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(!trace.contains("private"));
        assert!(!trace.chars().any(char::is_control));
        assert!(worker.authorized_workspace_ids().is_empty());
        assert_eq!(
            worker.relationship_owner().unwrap(),
            MemoryOwner::Relationship {
                user_id: UserId::new("alice"),
                agent_id: AgentId::new("a1"),
            }
        );
    }

    #[tokio::test]
    async fn retention_policy_applies_default_and_rejects_unbounded_lifetimes() {
        let policy = RelationshipMemoryRetentionPolicy::new(7, 2, 3, 1, 2, 10).unwrap();
        let store = InMemoryMemoryStore::with_retention_policy(policy);
        let ctx = worker(&session("alice", "a1", "s1"));
        let defaulted = store
            .append_relationship(&ctx, MemoryAppend::new("default"))
            .await
            .unwrap();
        assert_eq!(defaulted.retention_policy_revision, 7);
        assert_eq!(
            defaulted.expires_at.unwrap() - defaulted.created_at,
            2 * 24 * 60 * 60
        );
        assert!(
            store
                .append_relationship(&ctx, MemoryAppend::new("shorter").with_ttl(60))
                .await
                .is_ok()
        );
        assert!(matches!(
            store
                .append_relationship(
                    &ctx,
                    MemoryAppend::new("too long").with_ttl(4 * 24 * 60 * 60)
                )
                .await,
            Err(MemoryStoreError::InvalidInput)
        ));
        assert!(matches!(
            store
                .update_relationship(
                    &ctx,
                    &defaulted.id,
                    defaulted.revision,
                    MemoryPatch {
                        expiry: Some(MemoryExpiryPatch::Never),
                        ..MemoryPatch::default()
                    },
                )
                .await,
            Err(MemoryStoreError::InvalidInput)
        ));
    }
}
