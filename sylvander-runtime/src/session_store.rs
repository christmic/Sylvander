//! Session persistence — ephemeral vs persistent sessions.
//!
//! The [`SessionStore`] trait defines how sessions are persisted across
//! system restarts. Persistent sessions (group chats, channels) survive
//! restarts; ephemeral sessions are per-conversation and discarded.
//!
//! # Protocol metadata
//!
//! [`StoredSession::external_meta`] holds protocol-specific metadata
//! (Telegram chat_id, TUI window coordinates) that agents never see.
//! This is purely for the adapter layer to map external identifiers
//! to internal session IDs.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use sylvander_agent::session::SessionMetadata;
use sylvander_agent::spec::{AgentId, SessionId};

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
            created_at: sylvander_agent::session::now_secs(),
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
// SessionStore trait
// ---------------------------------------------------------------------------

/// Persistence backend for sessions.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// List all persistent sessions (loaded at boot).
    async fn list_persistent(&self) -> Result<Vec<StoredSession>, SessionStoreError>;

    /// Save or update a session record.
    async fn save(&self, session: &StoredSession) -> Result<(), SessionStoreError>;

    /// Delete a session record.
    async fn delete(&self, id: &SessionId) -> Result<(), SessionStoreError>;

    /// Look up a session by ID.
    async fn get(
        &self,
        id: &SessionId,
    ) -> Result<Option<StoredSession>, SessionStoreError>;
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
}

// ---------------------------------------------------------------------------
// InMemorySessionStore
// ---------------------------------------------------------------------------

/// In-memory session store (testing / development).
#[derive(Debug, Default)]
pub struct InMemorySessionStore {
    sessions: tokio::sync::RwLock<HashMap<SessionId, StoredSession>>,
}

impl InMemorySessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn list_persistent(&self) -> Result<Vec<StoredSession>, SessionStoreError> {
        let sessions = self.sessions.read().await;
        Ok(sessions
            .values()
            .filter(|s| s.lifetime == SessionLifetime::Persistent)
            .cloned()
            .collect())
    }

    async fn save(&self, session: &StoredSession) -> Result<(), SessionStoreError> {
        self.sessions
            .write()
            .await
            .insert(session.id.clone(), session.clone());
        Ok(())
    }

    async fn delete(&self, id: &SessionId) -> Result<(), SessionStoreError> {
        self.sessions.write().await.remove(id);
        Ok(())
    }

    async fn get(
        &self,
        id: &SessionId,
    ) -> Result<Option<StoredSession>, SessionStoreError> {
        Ok(self.sessions.read().await.get(id).cloned())
    }
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

    fn test_session(id: &str, lifetime: SessionLifetime) -> StoredSession {
        StoredSession::new(
            SessionId::new(id),
            format!("session-{id}"),
            lifetime,
            test_meta(),
            vec![AgentId::new("agent-1")],
        )
    }

    #[tokio::test]
    async fn list_persistent_filters_correctly() {
        let store = InMemorySessionStore::new();
        store
            .save(&test_session("s1", SessionLifetime::Persistent))
            .await
            .expect("save");
        store
            .save(&test_session("s2", SessionLifetime::Ephemeral))
            .await
            .expect("save");

        let persistent = store.list_persistent().await.expect("list");
        assert_eq!(persistent.len(), 1);
        assert_eq!(persistent[0].id, SessionId::new("s1"));
    }

    #[tokio::test]
    async fn save_and_get() {
        let store = InMemorySessionStore::new();
        store
            .save(&test_session("s1", SessionLifetime::Persistent))
            .await
            .expect("save");

        let found = store.get(&SessionId::new("s1")).await.expect("get");
        assert!(found.is_some());
    }

    #[tokio::test]
    async fn delete_removes() {
        let store = InMemorySessionStore::new();
        store
            .save(&test_session("s1", SessionLifetime::Ephemeral))
            .await
            .expect("save");
        store.delete(&SessionId::new("s1")).await.expect("delete");
        assert!(store.get(&SessionId::new("s1")).await.expect("get").is_none());
    }

    #[test]
    fn stored_session_with_external_meta() {
        let session = test_session("s1", SessionLifetime::Persistent)
            .with_external_meta("chat_id", serde_json::json!("-100xxx"));
        assert_eq!(
            session.external_meta.get("chat_id").unwrap(),
            &serde_json::json!("-100xxx")
        );
    }
}
