use std::sync::Arc;

use sylvander_agent::tools::{InMemoryMemoryStore, MemoryStore};

use crate::evidence::{EvidenceEncryption, EvidenceGovernance, EvidenceStore};
use crate::storage::RuntimeStorage;
use crate::storage::artifact::RuntimeArtifactService;
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

#[tokio::test]
async fn runtime_storage_owns_the_encrypted_artifact_authority() {
    let sessions: Arc<dyn SessionStore> = Arc::new(
        SqliteSessionStore::open_in_memory()
            .await
            .expect("open in-memory Session repository"),
    );
    let memory: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let encryption = EvidenceEncryption::from_secret("test-key", &[7; 32]).unwrap();
    let governance = EvidenceGovernance::new("tenant-a", 30, encryption).unwrap();
    let evidence = EvidenceStore::open_governed_in_memory(governance)
        .await
        .unwrap();
    let artifact_service = RuntimeArtifactService::new(evidence).unwrap();

    let storage =
        RuntimeStorage::new(sessions, memory).with_artifact_service(Some(artifact_service.clone()));

    assert!(storage.artifact_service().is_some());
    drop(artifact_service);
}
