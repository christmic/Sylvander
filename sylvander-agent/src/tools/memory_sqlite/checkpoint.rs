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

impl MemoryEvidenceCompactionReport {
    #[must_use]
    pub const fn total_deleted_count(&self) -> u32 {
        self.audit_deleted_count + self.retention_run_deleted_count
    }
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
#[path = "../../../tests/unit/tools_memory_sqlite_checkpoint.rs"]
mod tests;

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
