use std::path::Path;
use std::sync::{Arc, Barrier};

use rusqlite::Connection;
use sylvander_protocol::SessionContext;

use super::*;
use crate::tools::memory::{
    MemoryAppend, MemoryExecutionContext, MemoryStore, RelationshipMemoryRetentionPolicy,
};
use crate::tools::memory_sqlite::{MemoryIntegrityConfig, SqliteMemoryAdmin, SqliteMemoryStore};

const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

fn policy(batch: u32) -> RelationshipMemoryRetentionPolicy {
    RelationshipMemoryRetentionPolicy::new(1, 2, 3, 0, 0, batch).unwrap()
}

fn integrity(anchor: &Path) -> MemoryIntegrityConfig {
    MemoryIntegrityConfig::new(anchor, KEY).unwrap()
}

fn open(database: &Path, anchor: &Path, batch: u32) -> SqliteMemoryStore {
    let store =
        SqliteMemoryStore::open_with_integrity(database, policy(batch), integrity(anchor)).unwrap();
    store
        .maintenance()
        .activate_staged_retention_policy()
        .unwrap();
    store
}

fn worker() -> MemoryExecutionContext {
    MemoryExecutionContext::application_worker(&SessionContext::new(
        "alice",
        "agent-a",
        "session-a",
    ))
}

async fn evidence_fixture(
    directory: &Path,
    batch: u32,
) -> (SqliteMemoryStore, MemoryEvidenceCheckpoint) {
    let store = open(
        &directory.join("memory.db"),
        &directory.join("memory.anchor"),
        batch,
    );
    for content in ["one", "two", "three", "four", "five"] {
        store
            .append_relationship(&worker(), MemoryAppend::new(content))
            .await
            .unwrap();
    }
    for _ in 0..5 {
        store.maintenance().purge().unwrap();
    }
    let artifact = store.maintenance().backup_to_data_dir(directory).unwrap();
    (
        store,
        MemoryEvidenceCheckpoint::from_verified_backup(artifact),
    )
}

fn counts(store: &SqliteMemoryStore) -> (i64, i64, i64, i64) {
    store
            .with_raw_connection(|connection| {
                connection
                    .query_row(
                        "SELECT (SELECT COUNT(*) FROM relationship_memory_audit), (SELECT COUNT(*) FROM relationship_memory_retention_runs), (SELECT COUNT(*) FROM relationship_memory_retention_batches), (SELECT COUNT(*) FROM relationship_memory_checkpoint_state)",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .map_err(|_| checkpoint_error())
            })
            .unwrap()
}

fn boundaries(store: &SqliteMemoryStore) -> (String, String) {
    store
            .with_raw_connection(|connection| {
                let audit = connection
                    .query_row(
                        "SELECT event_id FROM relationship_memory_audit ORDER BY sequence DESC LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|_| checkpoint_error())?;
                let retention = connection
                    .query_row(
                        "SELECT run_id FROM relationship_memory_retention_runs ORDER BY completed_at DESC, run_id DESC LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|_| checkpoint_error())?;
                Ok((audit, retention))
            })
            .unwrap()
}

#[tokio::test]
async fn verified_checkpoint_compacts_in_batches_and_preserves_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let (store, checkpoint) = evidence_fixture(directory.path(), 2).await;
    assert_eq!(counts(&store), (5, 5, 5, 0));
    let preserved_boundaries = boundaries(&store);
    let report = store
        .maintenance()
        .compact_evidence_after_checkpoint(&checkpoint)
        .unwrap();
    assert_eq!(report.audit_deleted_count, 2);
    assert_eq!(report.retention_run_deleted_count, 2);
    assert_eq!(report.generation, 1);
    assert_eq!(counts(&store), (3, 3, 3, 1));
    assert_eq!(boundaries(&store), preserved_boundaries);

    let state: (i64, i64, String, i64, String) = store
            .with_raw_connection(|connection| {
                connection
                    .query_row(
                        "SELECT generation,audit_compacted_count,audit_summary_root,retention_compacted_count,retention_summary_root FROM relationship_memory_checkpoint_state",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                    )
                    .map_err(|_| checkpoint_error())
            })
            .unwrap();
    assert_eq!(state.0, 1);
    assert_eq!(state.1, 2);
    assert_eq!(state.2.len(), 64);
    assert_eq!(state.3, 2);
    assert_eq!(state.4.len(), 64);

    drop(store);
    let store = open(
        &directory.path().join("memory.db"),
        &directory.path().join("memory.anchor"),
        2,
    );
    let next = MemoryEvidenceCheckpoint::from_verified_backup(
        store
            .maintenance()
            .backup_to_data_dir(directory.path())
            .unwrap(),
    );
    store
        .maintenance()
        .compact_evidence_after_checkpoint(&next)
        .unwrap();
    assert_eq!(counts(&store), (1, 1, 1, 1));
    assert_eq!(boundaries(&store), preserved_boundaries);
    let cumulative: (i64, i64, i64) = store
            .with_raw_connection(|connection| {
                connection
                    .query_row(
                        "SELECT generation,audit_compacted_count,retention_compacted_count FROM relationship_memory_checkpoint_state",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .map_err(|_| checkpoint_error())
            })
            .unwrap();
    assert_eq!(cumulative, (2, 4, 4));
    let final_checkpoint = MemoryEvidenceCheckpoint::from_verified_backup(
        store
            .maintenance()
            .backup_to_data_dir(directory.path())
            .unwrap(),
    );
    let final_anchor = std::fs::read(directory.path().join("memory.anchor")).unwrap();
    let final_report = store
        .maintenance()
        .compact_evidence_after_checkpoint(&final_checkpoint)
        .unwrap();
    assert_eq!(final_report.audit_deleted_count, 0);
    assert_eq!(final_report.retention_run_deleted_count, 0);
    assert_eq!(counts(&store), (1, 1, 1, 1));
    assert_eq!(
        std::fs::read(directory.path().join("memory.anchor")).unwrap(),
        final_anchor
    );
}

#[tokio::test]
async fn missing_forged_and_old_checkpoints_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let (store, checkpoint) = evidence_fixture(directory.path(), 2).await;
    let missing = MemoryEvidenceCheckpoint::from_verified_backup(MemoryBackupArtifact {
        database_path: directory.path().join("missing.sqlite3"),
        manifest_path: directory.path().join("missing.manifest.json"),
        manifest: checkpoint.artifact.manifest.clone(),
    });
    assert!(
        store
            .maintenance()
            .compact_evidence_after_checkpoint(&missing)
            .is_err()
    );

    let forged_manifest = directory.path().join("forged.manifest.json");
    let mut forged = checkpoint.artifact.manifest.clone();
    forged.integrity_mac = "00".repeat(32);
    std::fs::write(&forged_manifest, serde_json::to_vec(&forged).unwrap()).unwrap();
    let forged = MemoryEvidenceCheckpoint::from_verified_backup(MemoryBackupArtifact {
        database_path: checkpoint.artifact.database_path.clone(),
        manifest_path: forged_manifest,
        manifest: forged,
    });
    assert!(
        store
            .maintenance()
            .compact_evidence_after_checkpoint(&forged)
            .is_err()
    );

    store
        .append_relationship(&worker(), MemoryAppend::new("new epoch"))
        .await
        .unwrap();
    let before = counts(&store);
    assert!(
        store
            .maintenance()
            .compact_evidence_after_checkpoint(&checkpoint)
            .is_err()
    );
    assert_eq!(counts(&store), before);
}

#[tokio::test]
async fn failed_transaction_rolls_back_deletes_state_and_anchor() {
    let directory = tempfile::tempdir().unwrap();
    let (store, checkpoint) = evidence_fixture(directory.path(), 2).await;
    let before_counts = counts(&store);
    let before_anchor = std::fs::read(directory.path().join("memory.anchor")).unwrap();
    assert!(
        store
            .maintenance()
            .compact_evidence_after_checkpoint_impl(&checkpoint, true)
            .is_err()
    );
    assert_eq!(counts(&store), before_counts);
    assert_eq!(
        std::fs::read(directory.path().join("memory.anchor")).unwrap(),
        before_anchor
    );
    drop(store);
    open(
        &directory.path().join("memory.db"),
        &directory.path().join("memory.anchor"),
        2,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_compaction_uses_anchor_epoch_as_cas() {
    let directory = tempfile::tempdir().unwrap();
    let (first, _) = evidence_fixture(directory.path(), 2).await;
    let second = open(
        &directory.path().join("memory.db"),
        &directory.path().join("memory.anchor"),
        2,
    );
    let checkpoint = MemoryEvidenceCheckpoint::from_verified_backup(
        first
            .maintenance()
            .backup_to_data_dir(directory.path())
            .unwrap(),
    );
    let barrier = Arc::new(Barrier::new(2));
    let first_checkpoint = checkpoint.clone();
    let first_barrier = Arc::clone(&barrier);
    let first_task = tokio::task::spawn_blocking(move || {
        first_barrier.wait();
        first
            .maintenance()
            .compact_evidence_after_checkpoint(&first_checkpoint)
    });
    let second_barrier = Arc::clone(&barrier);
    let second_task = tokio::task::spawn_blocking(move || {
        second_barrier.wait();
        second
            .maintenance()
            .compact_evidence_after_checkpoint(&checkpoint)
    });
    let outcomes = [first_task.await.unwrap(), second_task.await.unwrap()];
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_err()).count(),
        1
    );
}

#[tokio::test]
async fn direct_sql_cannot_compact_and_summary_tamper_is_detected() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let anchor = directory.path().join("memory.anchor");
    let (store, checkpoint) = evidence_fixture(directory.path(), 2).await;
    store
        .maintenance()
        .compact_evidence_after_checkpoint(&checkpoint)
        .unwrap();
    drop(store);

    let connection = Connection::open(&database).unwrap();
    assert!(
            connection
                .execute(
                    "DELETE FROM relationship_memory_audit WHERE sequence = (SELECT MIN(sequence) FROM relationship_memory_audit)",
                    [],
                )
                .is_err()
        );
    assert!(
            connection
                .execute(
                    "DELETE FROM relationship_memory_retention_batches WHERE batch_id = (SELECT MIN(batch_id) FROM relationship_memory_retention_batches)",
                    [],
                )
                .is_err()
        );
    assert!(
        connection
            .execute(
                "UPDATE relationship_memory_checkpoint_state SET audit_summary_root = ?1",
                ["00".repeat(32)],
            )
            .is_err()
    );
    connection
            .execute_batch(
                "DROP TRIGGER relationship_memory_checkpoint_no_update;
                 UPDATE relationship_memory_checkpoint_state SET audit_summary_root = '0000000000000000000000000000000000000000000000000000000000000000';",
            )
            .unwrap();
    drop(connection);
    assert!(
        SqliteMemoryStore::open_with_integrity(&database, policy(2), integrity(&anchor)).is_err()
    );
}

#[tokio::test]
async fn latest_post_compaction_checkpoint_restores_with_summary_chain() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let anchor = directory.path().join("memory.anchor");
    let (store, checkpoint) = evidence_fixture(directory.path(), 2).await;
    store
        .maintenance()
        .compact_evidence_after_checkpoint(&checkpoint)
        .unwrap();
    let latest = store
        .maintenance()
        .backup_to_data_dir(directory.path())
        .unwrap();
    drop(store);
    std::fs::remove_file(&database).unwrap();
    SqliteMemoryAdmin::restore_offline(
        &database,
        &latest.database_path,
        &latest.manifest_path,
        integrity(&anchor),
    )
    .unwrap();
    let restored = open(&database, &anchor, 2);
    assert_eq!(counts(&restored), (3, 3, 3, 1));
}
