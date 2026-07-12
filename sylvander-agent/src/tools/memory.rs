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
use sylvander_protocol::types::SessionId;

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
    pub owner: SessionContext,

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
    pub fn new(id: impl Into<String>, content: impl Into<String>, owner: SessionContext) -> Self {
        Self {
            id: id.into(),
            owner,
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
    pub fn with_tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
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
/// per-identity visibility: `search` and `get` only return entries
/// whose `owner.identity` matches the caller's.
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
        _ctx: &SessionContext,
        entry: MemoryEntry,
    ) -> Result<(), MemoryStoreError> {
        self.entries.write().await.push(entry);
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
        let entry = entries.iter().find(|e| e.id == id).cloned();
        Ok(entry.filter(|e| same_owner(&e.owner, ctx)))
    }
}

/// Two memories "belong to" the same identity if user + agent
/// match. (Session id is intentionally excluded — memories persist
/// across sessions by design.)
fn same_owner(a: &SessionContext, b: &SessionContext) -> bool {
    a.identity.user_id == b.identity.user_id && a.identity.agent_id == b.identity.agent_id
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
            .with_importance(Importance::High)
            .with_reference(MemoryReference::File {
                path: "/Cargo.toml".into(),
            });
        assert_eq!(entry.kind, MemoryKind::Decision);
        assert_eq!(entry.importance, Importance::High);
        assert_eq!(entry.references.len(), 1);
    }
}
