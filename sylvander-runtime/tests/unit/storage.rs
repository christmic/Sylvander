use std::sync::Arc;

use sylvander_agent::tools::{InMemoryMemoryStore, MemoryStore};

use crate::storage::RuntimeStorage;
use crate::storage::session::{SessionStore, SqliteSessionStore};

#[tokio::test]
async fn runtime_storage_retains_the_exact_composed_repositories() {
    let sessions: Arc<dyn SessionStore> = Arc::new(
        SqliteSessionStore::open_in_memory()
            .await
            .expect("open in-memory Session repository"),
    );
    let memory: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());

    let storage = RuntimeStorage::new(sessions.clone(), memory.clone());

    assert!(Arc::ptr_eq(storage.sessions(), &sessions));
    assert!(Arc::ptr_eq(storage.memory(), &memory));
}
