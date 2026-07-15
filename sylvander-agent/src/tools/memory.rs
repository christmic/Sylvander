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
//! The trait takes `&SessionContext` so the store can enforce per-user
//! isolation. Adding a new field to `MemoryEntry` (e.g. `confidence`,
//! `decay`) never breaks the trait — only the implementation.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sylvander_protocol::SessionContext;
use sylvander_protocol::types::{AgentId, SessionId, UserId};

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

    #[must_use]
    pub fn relationship(ctx: &SessionContext) -> Self {
        Self::Relationship {
            user_id: ctx.identity.user_id.clone(),
            agent_id: ctx.identity.agent_id.clone(),
        }
    }
}

impl From<SessionContext> for MemoryOwner {
    fn from(ctx: SessionContext) -> Self {
        Self::relationship(&ctx)
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

/// Identity snapshot issued by the runtime for one memory operation. It is not
/// part of any model-facing tool schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryExecutionContext {
    pub actor: MemoryActorKind,
    pub user_id: Option<UserId>,
    pub agent_id: Option<AgentId>,
    pub session_id: Option<SessionId>,
    pub authorized_workspace_ids: Vec<String>,
    pub trace_id: Option<String>,
}

impl MemoryExecutionContext {
    #[must_use]
    pub fn worker(session: &SessionContext) -> Self {
        Self {
            actor: MemoryActorKind::Worker,
            user_id: Some(session.identity.user_id.clone()),
            agent_id: Some(session.identity.agent_id.clone()),
            session_id: Some(session.identity.session_id.clone()),
            authorized_workspace_ids: Vec::new(),
            trace_id: session.request.trace_id.clone(),
        }
    }

    pub fn relationship_owner(&self) -> Result<MemoryOwner, MemoryStoreError> {
        if self.actor != MemoryActorKind::Worker {
            return Err(MemoryStoreError::AccessDenied(
                "actor cannot derive worker relationship ownership".into(),
            ));
        }
        let (Some(user_id), Some(agent_id), Some(_session_id)) =
            (&self.user_id, &self.agent_id, &self.session_id)
        else {
            return Err(MemoryStoreError::AccessDenied(
                "worker memory context is incomplete".into(),
            ));
        };
        Ok(MemoryOwner::Relationship {
            user_id: user_id.clone(),
            agent_id: agent_id.clone(),
        })
    }
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
}

impl MemoryEntry {
    /// Convenience: minimal entry with just an id, content, and
    /// owning session context. Kind defaults to `AgentNote`;
    /// importance to `Medium`.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        content: impl Into<String>,
        owner: impl Into<MemoryOwner>,
    ) -> Self {
        Self {
            id: id.into(),
            owner: owner.into(),
            kind: MemoryKind::AgentNote,
            content: content.into(),
            references: Vec::new(),
            tags: Vec::new(),
            importance: Importance::Medium,
            created_at: crate::session::now_secs(),
            last_accessed: None,
            access_count: 0,
            metadata: HashMap::new(),
        }
    }

    /// Builder-style: set the kind.
    #[must_use]
    pub fn with_kind(mut self, kind: MemoryKind) -> Self {
        self.kind = kind;
        self
    }

    /// Builder-style: add a tag.
    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Builder-style: add a single reference.
    #[must_use]
    pub fn with_reference(mut self, r: MemoryReference) -> Self {
        self.references.push(r);
        self
    }

    /// Builder-style: set importance.
    #[must_use]
    pub fn with_importance(mut self, importance: Importance) -> Self {
        self.importance = importance;
        self
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
    /// Search for entries owned by the calling identity and matching
    /// the query. Results are filtered by kind / importance if set.
    async fn search(
        &self,
        ctx: &SessionContext,
        query: &str,
        filter: MemoryFilter,
    ) -> Result<Vec<MemoryEntry>, MemoryStoreError>;

    /// Store a new entry. The entry's `owner` is what gets persisted;
    /// the `ctx` is just for authorization.
    async fn store(&self, ctx: &SessionContext, entry: MemoryEntry)
    -> Result<(), MemoryStoreError>;

    /// Delete an entry by ID. Refuses if the entry's owner doesn't
    /// match `ctx`.
    async fn delete(&self, ctx: &SessionContext, id: &str) -> Result<(), MemoryStoreError>;

    /// Get a single entry by ID. Refuses if the entry's owner
    /// doesn't match `ctx`.
    async fn get(
        &self,
        ctx: &SessionContext,
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
    #[error("access denied: {0}")]
    AccessDenied(String),
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
#[derive(Debug, Default)]
pub struct InMemoryMemoryStore {
    entries: tokio::sync::RwLock<Vec<MemoryEntry>>,
}

impl InMemoryMemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MemoryStore for InMemoryMemoryStore {
    async fn search(
        &self,
        ctx: &SessionContext,
        query: &str,
        filter: MemoryFilter,
    ) -> Result<Vec<MemoryEntry>, MemoryStoreError> {
        let query_lower = query.to_lowercase();
        let entries = self.entries.read().await;
        let mut results: Vec<MemoryEntry> = entries
            .iter()
            .filter(|e| same_owner(&e.owner, ctx))
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

    async fn store(
        &self,
        ctx: &SessionContext,
        entry: MemoryEntry,
    ) -> Result<(), MemoryStoreError> {
        if !same_owner(&entry.owner, ctx) {
            return Err(MemoryStoreError::AccessDenied(
                "memory owner does not match the runtime context".into(),
            ));
        }
        let mut entries = self.entries.write().await;
        if entries
            .iter()
            .any(|existing| existing.id == entry.id && same_owner(&existing.owner, ctx))
        {
            return Err(MemoryStoreError::Store(
                "memory identity already exists".into(),
            ));
        }
        entries.push(entry);
        Ok(())
    }

    async fn delete(&self, ctx: &SessionContext, id: &str) -> Result<(), MemoryStoreError> {
        let mut entries = self.entries.write().await;
        let before = entries.len();
        entries.retain(|e| !(e.id == id && same_owner(&e.owner, ctx)));
        if entries.len() == before {
            // Either not found, or not owned by ctx. Distinguish?
            // MVP: treat as access denied.
            return Err(MemoryStoreError::AccessDenied(format!(
                "no memory {id} visible to {}",
                ctx.identity.user_id
            )));
        }
        Ok(())
    }

    async fn get(
        &self,
        ctx: &SessionContext,
        id: &str,
    ) -> Result<Option<MemoryEntry>, MemoryStoreError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|entry| entry.id == id && same_owner(&entry.owner, ctx))
            .cloned())
    }
}

/// Two memories "belong to" the same identity if user + agent
/// match. (Session id is intentionally excluded — memories persist
/// across sessions by design.)
fn same_owner(owner: &MemoryOwner, ctx: &SessionContext) -> bool {
    owner == &MemoryOwner::relationship(ctx)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sylvander_protocol::types::{AgentId, UserId};

    fn alice_session() -> SessionContext {
        SessionContext::new(
            UserId::new("alice"),
            AgentId::new("a1"),
            SessionId::new("s1"),
        )
    }

    fn bob_session() -> SessionContext {
        SessionContext::new(UserId::new("bob"), AgentId::new("a1"), SessionId::new("s2"))
    }

    fn alice_other_agent_session() -> SessionContext {
        SessionContext::new(
            UserId::new("alice"),
            AgentId::new("a2"),
            SessionId::new("s3"),
        )
    }

    #[tokio::test]
    async fn store_and_search() {
        let store = InMemoryMemoryStore::new();
        let ctx = alice_session();
        store
            .store(
                &ctx,
                MemoryEntry::new("1", "The user prefers Rust", ctx.clone()),
            )
            .await
            .expect("store");

        let results = store
            .search(&ctx, "rust", MemoryFilter::default())
            .await
            .expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "The user prefers Rust");
    }

    #[tokio::test]
    async fn search_is_case_insensitive() {
        let store = InMemoryMemoryStore::new();
        let ctx = alice_session();
        store
            .store(&ctx, MemoryEntry::new("1", "RUST IS GREAT", ctx.clone()))
            .await
            .expect("store");
        let results = store
            .search(&ctx, "rust", MemoryFilter::default())
            .await
            .expect("search");
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn search_respects_limit() {
        let store = InMemoryMemoryStore::new();
        let ctx = alice_session();
        for i in 0..10 {
            store
                .store(
                    &ctx,
                    MemoryEntry::new(format!("{i}"), format!("item {i}"), ctx.clone()),
                )
                .await
                .expect("store");
        }
        let results = store
            .search(
                &ctx,
                "item",
                MemoryFilter {
                    limit: Some(3),
                    ..Default::default()
                },
            )
            .await
            .expect("search");
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn search_filters_by_kind() {
        let store = InMemoryMemoryStore::new();
        let ctx = alice_session();
        store
            .store(
                &ctx,
                MemoryEntry::new("1", "user likes dark mode", ctx.clone())
                    .with_kind(MemoryKind::Preference),
            )
            .await
            .unwrap();
        store
            .store(
                &ctx,
                MemoryEntry::new("2", "we chose tokio", ctx.clone())
                    .with_kind(MemoryKind::Decision),
            )
            .await
            .unwrap();

        let prefs = store
            .search(
                &ctx,
                "",
                MemoryFilter {
                    kind: Some(MemoryKind::Preference),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].id, "1");
    }

    #[tokio::test]
    async fn search_filters_by_min_importance() {
        let store = InMemoryMemoryStore::new();
        let ctx = alice_session();
        store
            .store(
                &ctx,
                MemoryEntry::new("1", "low", ctx.clone()).with_importance(Importance::Low),
            )
            .await
            .unwrap();
        store
            .store(
                &ctx,
                MemoryEntry::new("2", "high", ctx.clone()).with_importance(Importance::High),
            )
            .await
            .unwrap();

        let high = store
            .search(
                &ctx,
                "",
                MemoryFilter {
                    min_importance: Some(Importance::High),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(high.len(), 1);
        assert_eq!(high[0].id, "2");
    }

    #[tokio::test]
    async fn search_isolates_per_user() {
        let store = InMemoryMemoryStore::new();
        let alice = alice_session();
        let bob = bob_session();
        store
            .store(&alice, MemoryEntry::new("1", "alice secret", alice.clone()))
            .await
            .unwrap();
        store
            .store(&bob, MemoryEntry::new("2", "bob secret", bob.clone()))
            .await
            .unwrap();

        // Alice sees only her own.
        let alice_sees = store
            .search(&alice, "", MemoryFilter::default())
            .await
            .unwrap();
        assert_eq!(alice_sees.len(), 1);
        assert_eq!(alice_sees[0].id, "1");

        // Bob sees only his own.
        let bob_sees = store
            .search(&bob, "", MemoryFilter::default())
            .await
            .unwrap();
        assert_eq!(bob_sees.len(), 1);
        assert_eq!(bob_sees[0].id, "2");
    }

    #[tokio::test]
    async fn store_rejects_forged_user_or_agent_ownership() {
        let store = InMemoryMemoryStore::new();
        let alice = alice_session();
        for forged_owner in [bob_session(), alice_other_agent_session()] {
            let result = store
                .store(
                    &alice,
                    MemoryEntry::new("forged", "must not persist", forged_owner),
                )
                .await;
            assert!(matches!(result, Err(MemoryStoreError::AccessDenied(_))));
        }
        assert!(
            store
                .search(&alice, "", MemoryFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn same_id_is_isolated_by_user_and_agent_owner() {
        let store = InMemoryMemoryStore::new();
        let alice = alice_session();
        let bob = bob_session();
        let other_agent = alice_other_agent_session();
        for (owner, content) in [
            (&alice, "alice a1"),
            (&bob, "bob a1"),
            (&other_agent, "alice a2"),
        ] {
            store
                .store(owner, MemoryEntry::new("shared-id", content, owner.clone()))
                .await
                .unwrap();
        }
        assert_eq!(
            store
                .get(&alice, "shared-id")
                .await
                .unwrap()
                .unwrap()
                .content,
            "alice a1"
        );
        assert_eq!(
            store.get(&bob, "shared-id").await.unwrap().unwrap().content,
            "bob a1"
        );
        assert_eq!(
            store
                .get(&other_agent, "shared-id")
                .await
                .unwrap()
                .unwrap()
                .content,
            "alice a2"
        );
    }

    #[tokio::test]
    async fn delete_owned_entry() {
        let store = InMemoryMemoryStore::new();
        let ctx = alice_session();
        store
            .store(&ctx, MemoryEntry::new("1", "keep", ctx.clone()))
            .await
            .unwrap();
        store
            .store(&ctx, MemoryEntry::new("2", "drop", ctx.clone()))
            .await
            .unwrap();
        store.delete(&ctx, "2").await.expect("delete");
        let left: Vec<_> = store
            .search(&ctx, "", MemoryFilter::default())
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(left, vec!["1".to_string()]);
    }

    #[tokio::test]
    async fn delete_other_users_entry_is_denied() {
        let store = InMemoryMemoryStore::new();
        let alice = alice_session();
        let bob = bob_session();
        store
            .store(&alice, MemoryEntry::new("1", "alice", alice.clone()))
            .await
            .unwrap();
        // Bob tries to delete Alice's entry — should fail.
        let result = store.delete(&bob, "1").await;
        assert!(matches!(result, Err(MemoryStoreError::AccessDenied(_))));
        // Alice's entry is still there.
        let found = store.get(&alice, "1").await.unwrap();
        assert!(found.is_some());
    }

    #[tokio::test]
    async fn get_returns_none_for_other_user() {
        let store = InMemoryMemoryStore::new();
        let alice = alice_session();
        let bob = bob_session();
        store
            .store(&alice, MemoryEntry::new("1", "alice", alice.clone()))
            .await
            .unwrap();
        let found = store.get(&bob, "1").await.unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn memory_entry_with_kind_and_reference() {
        let ctx = alice_session();
        let entry = MemoryEntry::new("1", "we chose Rust", ctx)
            .with_kind(MemoryKind::Decision)
            .with_tag("architecture")
            .with_importance(Importance::High)
            .with_reference(MemoryReference::File {
                path: "/Cargo.toml".into(),
            });
        assert_eq!(entry.kind, MemoryKind::Decision);
        assert_eq!(entry.importance, Importance::High);
        assert_eq!(entry.references.len(), 1);
        assert_eq!(entry.tags, ["architecture"]);
    }

    #[test]
    fn runtime_context_derives_only_worker_relationship_ownership() {
        let session = alice_session();
        let worker = MemoryExecutionContext::worker(&session);
        assert_eq!(
            worker.relationship_owner().unwrap(),
            MemoryOwner::Relationship {
                user_id: UserId::new("alice"),
                agent_id: AgentId::new("a1"),
            }
        );
        let mut guardian = worker;
        guardian.actor = MemoryActorKind::Guardian;
        assert!(matches!(
            guardian.relationship_owner(),
            Err(MemoryStoreError::AccessDenied(_))
        ));
    }
}
