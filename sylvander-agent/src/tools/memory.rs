//! Memory store abstraction — pluggable backends for long-term agent memory.
//!
//! The [`MemoryStore`] trait defines the interface. Two implementations are
//! provided:
//! - [`InMemoryMemoryStore`] — for testing / ephemeral use
//! - `SqliteMemoryStore` — for production (future, not yet implemented)

use std::collections::HashMap;

use async_trait::async_trait;

// ---------------------------------------------------------------------------
// MemoryEntry
// ---------------------------------------------------------------------------

/// A single entry in an agent's long-term memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEntry {
    /// Unique identifier for this entry.
    pub id: String,
    /// The memory content (free-form text).
    pub content: String,
    /// Optional key-value metadata.
    pub metadata: HashMap<String, String>,
    /// Unix timestamp when this entry was created.
    pub created_at: i64,
}

impl MemoryEntry {
    /// Create a new memory entry with the current timestamp.
    #[must_use]
    pub fn new(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            metadata: HashMap::new(),
            created_at: crate::session::now_secs(),
        }
    }

    /// Add a metadata tag.
    #[must_use]
    pub fn with_tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

// ---------------------------------------------------------------------------
// MemoryStore trait
// ---------------------------------------------------------------------------

/// Backend for long-term agent memory.
///
/// Implementations can range from in-memory `Vec` to SQLite, vector DBs,
/// or external services.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Search for entries matching a query string.
    ///
    /// The search is implementation-defined. A simple implementation may
    /// do case-insensitive substring matching; a production implementation
    /// may use embeddings + vector search.
    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryStoreError>;

    /// Store a new entry.
    async fn store(&self, entry: MemoryEntry) -> Result<(), MemoryStoreError>;

    /// Delete an entry by ID.
    ///
    /// If the entry does not exist, the operation is a no-op (not an error).
    async fn delete(&self, id: &str) -> Result<(), MemoryStoreError>;
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
}

// ---------------------------------------------------------------------------
// InMemoryMemoryStore
// ---------------------------------------------------------------------------

/// An in-memory memory store backed by a `Vec<MemoryEntry>`.
///
/// Search is case-insensitive substring matching. Suitable for testing
/// and ephemeral use.
#[derive(Debug, Default)]
pub struct InMemoryMemoryStore {
    entries: tokio::sync::RwLock<Vec<MemoryEntry>>,
}

impl InMemoryMemoryStore {
    /// Create a new empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MemoryStore for InMemoryMemoryStore {
    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryStoreError> {
        let query_lower = query.to_lowercase();
        let entries = self.entries.read().await;
        let results: Vec<MemoryEntry> = entries
            .iter()
            .filter(|e| e.content.to_lowercase().contains(&query_lower))
            .take(limit)
            .cloned()
            .collect();
        Ok(results)
    }

    async fn store(&self, entry: MemoryEntry) -> Result<(), MemoryStoreError> {
        self.entries.write().await.push(entry);
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<(), MemoryStoreError> {
        self.entries.write().await.retain(|e| e.id != id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn store_and_search() {
        let store = InMemoryMemoryStore::new();

        store
            .store(MemoryEntry::new("1", "The user prefers Rust"))
            .await
            .expect("store");
        store
            .store(MemoryEntry::new("2", "Project uses PostgreSQL"))
            .await
            .expect("store");
        store
            .store(MemoryEntry::new("3", "The user likes crabs"))
            .await
            .expect("store");

        let results = store.search("rust", 5).await.expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "The user prefers Rust");
    }

    #[tokio::test]
    async fn search_is_case_insensitive() {
        let store = InMemoryMemoryStore::new();
        store
            .store(MemoryEntry::new("1", "RUST IS GREAT"))
            .await
            .expect("store");

        let results = store.search("rust", 5).await.expect("search");
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn search_respects_limit() {
        let store = InMemoryMemoryStore::new();
        for i in 0..10 {
            store
                .store(MemoryEntry::new(format!("{i}"), format!("item {i}")))
                .await
                .expect("store");
        }

        let results = store.search("item", 3).await.expect("search");
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn delete_removes_entry() {
        let store = InMemoryMemoryStore::new();
        store
            .store(MemoryEntry::new("keep", "keep me"))
            .await
            .expect("store");
        store
            .store(MemoryEntry::new("drop", "drop me"))
            .await
            .expect("store");

        store.delete("drop").await.expect("delete");

        let results = store.search("drop", 5).await.expect("search");
        assert!(results.is_empty());

        let results = store.search("keep", 5).await.expect("search");
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn delete_nonexistent_is_noop() {
        let store = InMemoryMemoryStore::new();
        store.delete("nonexistent").await.expect("delete");
        // Should not panic or error
    }

    #[test]
    fn memory_entry_with_tag() {
        let entry = MemoryEntry::new("1", "content")
            .with_tag("category", "preference")
            .with_tag("priority", "high");
        assert_eq!(entry.metadata.get("category").unwrap(), "preference");
        assert_eq!(entry.metadata.get("priority").unwrap(), "high");
    }
}
