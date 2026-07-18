use super::*;
use crate::tools::memory::{MemoryAppend, MemoryExecutionContext, MemoryFilter, MemoryStore};
use crate::tools::memory_sqlite::{RelationshipMemoryRetentionPolicy, SqliteMemoryStore};
use std::path::Path;
use sylvander_protocol::SessionContext;

const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

fn config(path: &Path) -> MemoryIntegrityConfig {
    MemoryIntegrityConfig::new(path, KEY).unwrap()
}

fn worker() -> MemoryExecutionContext {
    MemoryExecutionContext::application_worker(&SessionContext::new(
        "alice",
        "agent-a",
        "session-a",
    ))
}

fn open(database: &Path, anchor: &Path) -> Result<SqliteMemoryStore, MemoryStoreError> {
    let store = SqliteMemoryStore::open_with_integrity(
        database,
        RelationshipMemoryRetentionPolicy::default(),
        config(anchor),
    )?;
    store.maintenance().activate_staged_retention_policy()?;
    Ok(store)
}

#[tokio::test]
async fn authenticated_anchor_survives_mutation_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let anchor = directory.path().join("anchor/state.json");
    let store = open(&database, &anchor).unwrap();
    store
        .append_relationship(&worker(), MemoryAppend::new("durable"))
        .await
        .unwrap();
    let anchored = std::fs::read(&anchor).unwrap();
    store
        .search_relationship(&worker(), "durable", MemoryFilter::default())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&anchor).unwrap(), anchored);
    drop(store);
    open(&database, &anchor).unwrap();
    let record: AnchorRecord = serde_json::from_slice(&std::fs::read(&anchor).unwrap()).unwrap();
    assert!(matches!(record, AnchorRecord::Committed { epoch, .. } if epoch > 1));
    let encoded = std::fs::read_to_string(anchor).unwrap();
    assert!(!encoded.contains(std::str::from_utf8(KEY).unwrap()));
    assert!(!encoded.contains("durable"));
}

#[tokio::test]
async fn row_tamper_audit_deletion_and_database_rollback_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let anchor = directory.path().join("anchor.json");
    let store = open(&database, &anchor).unwrap();
    store
        .append_relationship(&worker(), MemoryAppend::new("first"))
        .await
        .unwrap();
    drop(store);
    let old_database = std::fs::read(&database).unwrap();

    let store = open(&database, &anchor).unwrap();
    store
        .append_relationship(&worker(), MemoryAppend::new("second"))
        .await
        .unwrap();
    drop(store);

    std::fs::write(&database, &old_database).unwrap();
    let error = open(&database, &anchor).unwrap_err();
    assert_eq!(
        error.to_string(),
        "store error: memory integrity verification failed"
    );

    // Restore the current database by making a fresh protected fixture,
    // then simulate direct row tampering that leaves the schema intact.
    let database = directory.path().join("tampered.db");
    let anchor = directory.path().join("tampered.anchor");
    let store = open(&database, &anchor).unwrap();
    store
        .append_relationship(&worker(), MemoryAppend::new("original"))
        .await
        .unwrap();
    drop(store);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("UPDATE relationship_memories SET content = 'forged'", [])
        .unwrap();
    drop(connection);
    let error = open(&database, &anchor).unwrap_err();
    assert_eq!(
        error.to_string(),
        "store error: memory integrity verification failed"
    );

    // Audit deletion requires removing the append-only trigger. Exact
    // schema verification rejects that attack before content is exposed.
    let database = directory.path().join("audit-tampered.db");
    let anchor = directory.path().join("audit-tampered.anchor");
    let store = open(&database, &anchor).unwrap();
    store
        .append_relationship(&worker(), MemoryAppend::new("original"))
        .await
        .unwrap();
    drop(store);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "DROP TRIGGER relationship_memory_audit_no_delete;\
                 DELETE FROM relationship_memory_audit;",
        )
        .unwrap();
    drop(connection);
    let error = open(&database, &anchor).unwrap_err();
    assert_eq!(
        error.to_string(),
        "store error: unsupported relationship memory schema"
    );
}

#[tokio::test]
async fn live_recall_rejects_forged_row_without_scanning_or_resealing_anchor() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let anchor = directory.path().join("anchor.json");
    let store = open(&database, &anchor).unwrap();
    store
        .append_relationship(&worker(), MemoryAppend::new("trusted"))
        .await
        .unwrap();
    let anchored = std::fs::read(&anchor).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("UPDATE relationship_memories SET content = 'forged'", [])
        .unwrap();
    drop(connection);

    let error = store
        .search_relationship(&worker(), "", MemoryFilter::default())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "search error: memory search failed");
    assert_eq!(std::fs::read(anchor).unwrap(), anchored);
}

#[test]
fn missing_modified_or_wrong_key_anchor_is_rejected_without_secret_disclosure() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let anchor = directory.path().join("anchor.json");
    drop(open(&database, &anchor).unwrap());

    let wrong = MemoryIntegrityConfig::new(&anchor, b"abcdef0123456789abcdef0123456789").unwrap();
    let error = SqliteMemoryStore::open_with_integrity(
        &database,
        RelationshipMemoryRetentionPolicy::default(),
        wrong,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "store error: memory integrity verification failed"
    );
    assert!(!format!("{error:?}").contains("abcdef"));

    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&anchor).unwrap()).unwrap();
    record["epoch"] = serde_json::json!(999);
    std::fs::write(&anchor, serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(open(&database, &anchor).is_err());
    std::fs::remove_file(&anchor).unwrap();
    assert!(open(&database, &anchor).is_err());
}

#[tokio::test]
async fn pending_anchor_recovers_only_prepared_rollback_or_commit_roots() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let anchor = directory.path().join("anchor.json");
    let store = open(&database, &anchor).unwrap();
    store
        .append_relationship(&worker(), MemoryAppend::new("before"))
        .await
        .unwrap();
    drop(store);

    let integrity = IntegrityState::new(config(&anchor));
    let mut connection = Connection::open(&database).unwrap();
    let before = integrity.verify(&connection).unwrap();
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    transaction
        .execute("UPDATE relationship_memories SET content = 'after'", [])
        .unwrap();
    let after = database_root(&transaction).unwrap();
    integrity.prepare(&before, &after).unwrap();
    transaction.commit().unwrap();
    drop(connection);
    drop(integrity);

    // Simulates a crash after SQLite commit and before finalize.
    drop(open(&database, &anchor).unwrap());
    let record: AnchorRecord = serde_json::from_slice(&std::fs::read(&anchor).unwrap()).unwrap();
    assert!(matches!(record, AnchorRecord::Committed { epoch, .. } if epoch > 1));

    let integrity = IntegrityState::new(config(&anchor));
    let mut connection = Connection::open(&database).unwrap();
    let before = integrity.verify(&connection).unwrap();
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    transaction
        .execute(
            "UPDATE relationship_memories SET content = 'rolled-back'",
            [],
        )
        .unwrap();
    let after = database_root(&transaction).unwrap();
    integrity.prepare(&before, &after).unwrap();
    transaction.rollback().unwrap();
    drop(connection);
    drop(integrity);

    // Simulates a crash after prepare and before SQLite commit.
    drop(open(&database, &anchor).unwrap());
}
