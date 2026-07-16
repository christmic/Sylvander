//! Verified-checkpoint authorization for bounded evidence compaction.

use std::fmt::Write as _;

use rusqlite::{OptionalExtension, params, types::ValueRef};
use sha2::{Digest, Sha256};

use super::{
    CheckpointCompactionGuard, MemoryBackupArtifact, MemoryStoreError, SqliteMemoryMaintenance,
    backup, checkpoint_error,
};

const AUDIT_GENESIS_DOMAIN: &[u8] = b"sylvander-memory-audit-checkpoint-genesis-v1";
const AUDIT_CHUNK_DOMAIN: &[u8] = b"sylvander-memory-audit-checkpoint-chunk-v1";
const RETENTION_GENESIS_DOMAIN: &[u8] = b"sylvander-memory-retention-checkpoint-genesis-v1";
const RETENTION_CHUNK_DOMAIN: &[u8] = b"sylvander-memory-retention-checkpoint-chunk-v1";

/// A verified backup selected as the evidence boundary for one bounded
/// compaction. The artifact is re-read and re-verified inside the write
/// transaction; this wrapper does not grant authority by itself.
#[derive(Debug, Clone)]
pub struct MemoryEvidenceCheckpoint {
    artifact: MemoryBackupArtifact,
}

impl MemoryEvidenceCheckpoint {
    #[must_use]
    pub fn from_verified_backup(artifact: MemoryBackupArtifact) -> Self {
        Self { artifact }
    }
}

/// Content-free result of one checkpoint-authorized bounded transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEvidenceCompactionReport {
    pub checkpoint_epoch: u64,
    pub generation: u64,
    pub audit_deleted_count: u32,
    pub retention_run_deleted_count: u32,
}

#[derive(Debug)]
struct AuditChunk {
    sequences: Vec<i64>,
    root: String,
}

#[derive(Debug)]
struct RetentionChunk {
    pairs: Vec<(String, String)>,
    root: String,
}

#[derive(Debug)]
struct CheckpointState {
    generation: u64,
    audit_count: u64,
    audit_root: String,
    retention_count: u64,
    retention_root: String,
}

impl SqliteMemoryMaintenance {
    /// Deletes at most one retention-policy batch of old evidence after a
    /// signed backup proves that the exact current anchored state was
    /// published. The newest audit row and retention run remain as live
    /// continuity boundaries. Removed rows are folded into authenticated,
    /// domain-separated summary roots before the transaction commits.
    pub fn compact_evidence_after_checkpoint(
        &self,
        checkpoint: &MemoryEvidenceCheckpoint,
    ) -> Result<MemoryEvidenceCompactionReport, MemoryStoreError> {
        self.compact_evidence_after_checkpoint_impl(checkpoint, false)
    }

    fn compact_evidence_after_checkpoint_impl(
        &self,
        checkpoint: &MemoryEvidenceCheckpoint,
        fail_before_state: bool,
    ) -> Result<MemoryEvidenceCompactionReport, MemoryStoreError> {
        let limit = self.store.retention_policy()?.batch_limit();
        let now = self.store.clock.now_secs();
        self.store.with_connection(|transaction| {
            let verified = backup::verify_current_checkpoint(
                &self.store,
                transaction,
                &checkpoint.artifact,
            )
            .map_err(|_| checkpoint_error())?;
            let previous = load_state(transaction)?;
            let audit = collect_audit_chunk(transaction, limit, &previous.audit_root)?;
            let retention =
                collect_retention_chunk(transaction, limit, &previous.retention_root)?;
            if audit.sequences.is_empty() && retention.pairs.is_empty() {
                return Ok(MemoryEvidenceCompactionReport {
                    checkpoint_epoch: verified.epoch,
                    generation: previous.generation,
                    audit_deleted_count: 0,
                    retention_run_deleted_count: 0,
                });
            }

            let audit_deleted_count = u32::try_from(audit.sequences.len())
                .map_err(|_| checkpoint_error())?;
            let retention_run_deleted_count =
                u32::try_from(retention.pairs.len()).map_err(|_| checkpoint_error())?;
            let generation = previous
                .generation
                .checked_add(1)
                .ok_or_else(checkpoint_error)?;
            let audit_count = previous
                .audit_count
                .checked_add(u64::from(audit_deleted_count))
                .ok_or_else(checkpoint_error)?;
            let retention_count = previous
                .retention_count
                .checked_add(u64::from(retention_run_deleted_count))
                .ok_or_else(checkpoint_error)?;

            let _guard = CheckpointCompactionGuard::enter()?;
            for sequence in &audit.sequences {
                if transaction
                    .execute(
                        "DELETE FROM relationship_memory_audit WHERE sequence = ?1",
                        [sequence],
                    )
                    .map_err(|_| checkpoint_error())?
                    != 1
                {
                    return Err(checkpoint_error());
                }
            }
            for (_, batch_id) in &retention.pairs {
                if transaction
                    .execute(
                        "DELETE FROM relationship_memory_retention_batches WHERE batch_id = ?1",
                        [batch_id],
                    )
                    .map_err(|_| checkpoint_error())?
                    != 1
                {
                    return Err(checkpoint_error());
                }
            }
            for (run_id, _) in &retention.pairs {
                if transaction
                    .execute(
                        "DELETE FROM relationship_memory_retention_runs WHERE run_id = ?1 AND NOT EXISTS (SELECT 1 FROM relationship_memory_retention_batches WHERE run_id = ?1)",
                        [run_id],
                    )
                    .map_err(|_| checkpoint_error())?
                    != 1
                {
                    return Err(checkpoint_error());
                }
            }
            if fail_before_state {
                return Err(checkpoint_error());
            }
            let changed = transaction
                .execute(
                    "INSERT INTO relationship_memory_checkpoint_state (singleton, generation, checkpoint_epoch, checkpoint_root, checkpoint_sha256, audit_compacted_count, audit_summary_root, retention_compacted_count, retention_summary_root, updated_at) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(singleton) DO UPDATE SET generation = excluded.generation, checkpoint_epoch = excluded.checkpoint_epoch, checkpoint_root = excluded.checkpoint_root, checkpoint_sha256 = excluded.checkpoint_sha256, audit_compacted_count = excluded.audit_compacted_count, audit_summary_root = excluded.audit_summary_root, retention_compacted_count = excluded.retention_compacted_count, retention_summary_root = excluded.retention_summary_root, updated_at = excluded.updated_at WHERE relationship_memory_checkpoint_state.generation = ?10",
                    params![
                        i64::try_from(generation).map_err(|_| checkpoint_error())?,
                        i64::try_from(verified.epoch).map_err(|_| checkpoint_error())?,
                        verified.database_root,
                        verified.sha256,
                        i64::try_from(audit_count).map_err(|_| checkpoint_error())?,
                        audit.root,
                        i64::try_from(retention_count).map_err(|_| checkpoint_error())?,
                        retention.root,
                        now,
                        i64::try_from(previous.generation).map_err(|_| checkpoint_error())?,
                    ],
                )
                .map_err(|_| checkpoint_error())?;
            if changed != 1 {
                return Err(MemoryStoreError::Conflict);
            }
            Ok(MemoryEvidenceCompactionReport {
                checkpoint_epoch: verified.epoch,
                generation,
                audit_deleted_count,
                retention_run_deleted_count,
            })
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::items_after_test_module,
    reason = "checkpoint tests need private fault injection while hashing helpers stay grouped below"
)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Barrier};

    use rusqlite::Connection;
    use sylvander_protocol::SessionContext;

    use super::*;
    use crate::tools::memory::{
        MemoryAppend, MemoryExecutionContext, MemoryStore, RelationshipMemoryRetentionPolicy,
    };
    use crate::tools::memory_sqlite::{
        MemoryIntegrityConfig, SqliteMemoryAdmin, SqliteMemoryStore,
    };

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn policy(batch: u32) -> RelationshipMemoryRetentionPolicy {
        RelationshipMemoryRetentionPolicy::new(1, 2, 3, 0, 0, batch).unwrap()
    }

    fn integrity(anchor: &Path) -> MemoryIntegrityConfig {
        MemoryIntegrityConfig::new(anchor, KEY).unwrap()
    }

    fn open(database: &Path, anchor: &Path, batch: u32) -> SqliteMemoryStore {
        let store =
            SqliteMemoryStore::open_with_integrity(database, policy(batch), integrity(anchor))
                .unwrap();
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
        let final_report = store
            .maintenance()
            .compact_evidence_after_checkpoint(&final_checkpoint)
            .unwrap();
        assert_eq!(final_report.audit_deleted_count, 0);
        assert_eq!(final_report.retention_run_deleted_count, 0);
        assert_eq!(counts(&store), (1, 1, 1, 1));
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
            SqliteMemoryStore::open_with_integrity(&database, policy(2), integrity(&anchor))
                .is_err()
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
}

fn load_state(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<CheckpointState, MemoryStoreError> {
    transaction
        .query_row(
            "SELECT generation, audit_compacted_count, audit_summary_root, retention_compacted_count, retention_summary_root FROM relationship_memory_checkpoint_state WHERE singleton = 1",
            [],
            |row| {
                Ok(CheckpointState {
                    generation: read_non_negative(row.get(0)?)?,
                    audit_count: read_non_negative(row.get(1)?)?,
                    audit_root: row.get(2)?,
                    retention_count: read_non_negative(row.get(3)?)?,
                    retention_root: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|_| checkpoint_error())
        .map(|state| {
            state.unwrap_or_else(|| CheckpointState {
                generation: 0,
                audit_count: 0,
                audit_root: hex_digest(AUDIT_GENESIS_DOMAIN),
                retention_count: 0,
                retention_root: hex_digest(RETENTION_GENESIS_DOMAIN),
            })
        })
}

fn collect_audit_chunk(
    transaction: &rusqlite::Transaction<'_>,
    limit: u32,
    previous_root: &str,
) -> Result<AuditChunk, MemoryStoreError> {
    let boundary: Option<i64> = transaction
        .query_row(
            "SELECT MAX(sequence) FROM relationship_memory_audit",
            [],
            |row| row.get(0),
        )
        .map_err(|_| checkpoint_error())?;
    let Some(boundary) = boundary else {
        return Ok(AuditChunk {
            sequences: Vec::new(),
            root: previous_root.into(),
        });
    };
    let mut digest = chunk_digest(AUDIT_CHUNK_DOMAIN, previous_root)?;
    let mut statement = transaction
        .prepare("SELECT sequence,event_id,occurred_at,operation,target_record_key,before_revision,after_revision,actor_kind,actor_user_id,actor_agent_id,session_id,trace_id,changed_mask FROM relationship_memory_audit WHERE sequence < ?1 ORDER BY sequence LIMIT ?2")
        .map_err(|_| checkpoint_error())?;
    let mut rows = statement
        .query(params![boundary, limit])
        .map_err(|_| checkpoint_error())?;
    let mut sequences = Vec::new();
    while let Some(row) = rows.next().map_err(|_| checkpoint_error())? {
        sequences.push(row.get(0).map_err(|_| checkpoint_error())?);
        hash_row(&mut digest, row, 13)?;
    }
    Ok(AuditChunk {
        sequences,
        root: hex(&digest.finalize()),
    })
}

fn collect_retention_chunk(
    transaction: &rusqlite::Transaction<'_>,
    limit: u32,
    previous_root: &str,
) -> Result<RetentionChunk, MemoryStoreError> {
    let boundary: Option<(i64, String)> = transaction
        .query_row(
            "SELECT completed_at, run_id FROM relationship_memory_retention_runs ORDER BY completed_at DESC, run_id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| checkpoint_error())?;
    let Some((completed_at, run_id)) = boundary else {
        return Ok(RetentionChunk {
            pairs: Vec::new(),
            root: previous_root.into(),
        });
    };
    let mut digest = chunk_digest(RETENTION_CHUNK_DOMAIN, previous_root)?;
    let mut statement = transaction
        .prepare("SELECT r.run_id,r.started_at,r.completed_at,r.policy_revision,r.clock_watermark,r.expired_count,r.superseded_count,b.batch_id,b.occurred_at,b.attempted_limit,b.expired_count,b.superseded_count FROM relationship_memory_retention_runs r JOIN relationship_memory_retention_batches b ON b.run_id = r.run_id WHERE r.completed_at < ?1 OR (r.completed_at = ?1 AND r.run_id < ?2) ORDER BY r.completed_at, r.run_id LIMIT ?3")
        .map_err(|_| checkpoint_error())?;
    let mut rows = statement
        .query(params![completed_at, run_id, limit])
        .map_err(|_| checkpoint_error())?;
    let mut pairs = Vec::new();
    while let Some(row) = rows.next().map_err(|_| checkpoint_error())? {
        pairs.push((
            row.get(0).map_err(|_| checkpoint_error())?,
            row.get(7).map_err(|_| checkpoint_error())?,
        ));
        hash_row(&mut digest, row, 12)?;
    }
    Ok(RetentionChunk {
        pairs,
        root: hex(&digest.finalize()),
    })
}

fn chunk_digest(domain: &[u8], previous_root: &str) -> Result<Sha256, MemoryStoreError> {
    if previous_root.len() != 64 {
        return Err(checkpoint_error());
    }
    let mut digest = Sha256::new();
    digest.update(domain);
    hash_field(&mut digest, previous_root.as_bytes());
    Ok(digest)
}

fn hash_row(
    digest: &mut Sha256,
    row: &rusqlite::Row<'_>,
    columns: usize,
) -> Result<(), MemoryStoreError> {
    digest.update(b"R");
    for index in 0..columns {
        match row.get_ref(index).map_err(|_| checkpoint_error())? {
            ValueRef::Null => digest.update(b"N"),
            ValueRef::Integer(value) => {
                digest.update(b"I");
                digest.update(value.to_be_bytes());
            }
            ValueRef::Real(value) => {
                digest.update(b"F");
                digest.update(value.to_bits().to_be_bytes());
            }
            ValueRef::Text(value) => {
                digest.update(b"T");
                hash_field(digest, value);
            }
            ValueRef::Blob(value) => {
                digest.update(b"B");
                hash_field(digest, value);
            }
        }
    }
    Ok(())
}

fn read_non_negative(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| rusqlite::Error::InvalidQuery)
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn hex_digest(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}
