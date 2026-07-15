use super::*;
use crate::tools::memory::{MemoryAppend, MemoryExecutionContext};
use sylvander_protocol::SessionContext;

fn worker() -> MemoryExecutionContext {
    MemoryExecutionContext::application_worker(&SessionContext::new("alice", "agent-a", "session"))
}

fn policy(revision: u64, batch: u32) -> RelationshipMemoryRetentionPolicy {
    RelationshipMemoryRetentionPolicy::new(revision, 2, 3, 0, 0, batch).unwrap()
}

#[tokio::test]
async fn purge_is_bounded_audited_and_restart_safe() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let store = SqliteMemoryStore::open_with_retention_policy(file.path(), policy(7, 2)).unwrap();
    let ctx = worker();
    let mut entries = Vec::new();
    for content in ["one", "two", "three"] {
        entries.push(
            store
                .append_relationship(&ctx, MemoryAppend::new(content))
                .await
                .unwrap(),
        );
    }
    let now = crate::session::now_secs();
    store
        .with_connection(|connection| {
            connection
                .execute(
                    "UPDATE relationship_memories SET expires_at = ?1",
                    [now - 1],
                )
                .map_err(store_error)?;
            Ok(())
        })
        .unwrap();
    let maintenance = store.maintenance();
    assert_eq!(
        maintenance.purge_at(now).unwrap(),
        MemoryPurgeReport {
            expired_count: 2,
            superseded_count: 0
        }
    );
    assert_eq!(maintenance.purge_at(now).unwrap().total_count(), 1);
    assert_eq!(maintenance.purge_at(now).unwrap().total_count(), 0);
    store
        .with_connection(|connection| {
            let state: (i64, i64, i64, i64) = connection
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM relationship_memories), (SELECT COUNT(*) FROM relationship_memory_audit WHERE operation = 'purge_expired'), (SELECT COUNT(*) FROM relationship_memory_retention_runs), (SELECT COUNT(*) FROM relationship_memory_retention_batches)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(store_error)?;
            assert_eq!(state, (0, 3, 3, 3));
            Ok(())
        })
        .unwrap();
    drop(store);

    let reopened =
        SqliteMemoryStore::open_with_retention_policy(file.path(), policy(7, 2)).unwrap();
    let fresh = reopened
        .append_relationship(&ctx, MemoryAppend::new("fresh"))
        .await
        .unwrap();
    assert_eq!(fresh.retention_policy_revision, 7);
    assert_eq!(
        fresh.expires_at.unwrap() - fresh.created_at,
        2 * 24 * 60 * 60
    );
    drop(reopened);
    let reopened =
        SqliteMemoryStore::open_with_retention_policy(file.path(), policy(7, 2)).unwrap();
    let loaded = reopened
        .get_relationship(&ctx, &fresh.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.expires_at, fresh.expires_at);
    assert_eq!(loaded.retention_policy_revision, 7);
    drop(reopened);
    assert!(SqliteMemoryStore::open_with_retention_policy(file.path(), policy(6, 2)).is_err());
}

#[tokio::test]
async fn superseded_rows_purge_without_dangling_the_replacement() {
    let store = SqliteMemoryStore::open_in_memory_with_retention_policy(policy(1, 10)).unwrap();
    let ctx = worker();
    let original = store
        .append_relationship(&ctx, MemoryAppend::new("old"))
        .await
        .unwrap();
    let replacement = store
        .supersede_relationship(
            &ctx,
            &original.id,
            original.revision,
            MemoryAppend::new("new"),
        )
        .await
        .unwrap();
    let report = store
        .maintenance()
        .purge_at(crate::session::now_secs())
        .unwrap();
    assert_eq!(report.superseded_count, 1);
    assert!(
        store
            .get_relationship(&ctx, &replacement.id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn maintenance_fault_rolls_back_rows_audit_ledgers_and_watermark() {
    let store = SqliteMemoryStore::open_in_memory_with_retention_policy(policy(1, 10)).unwrap();
    let ctx = worker();
    store
        .append_relationship(&ctx, MemoryAppend::new("keep on failure"))
        .await
        .unwrap();
    let now = crate::session::now_secs();
    let before = store
        .with_connection(|connection| {
            connection
                .execute(
                    "UPDATE relationship_memories SET expires_at = ?1",
                    [now - 1],
                )
                .map_err(store_error)?;
            connection
                .execute_batch(
                    "CREATE TRIGGER reject_purge_audit BEFORE INSERT ON relationship_memory_audit WHEN NEW.operation LIKE 'purge_%' BEGIN SELECT RAISE(ABORT, 'secret row content'); END;",
                )
                .map_err(store_error)?;
            connection
                .query_row(
                    "SELECT clock_watermark FROM relationship_memory_retention_state",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(store_error)
        })
        .unwrap();
    let error = store.maintenance().purge_at(now + 1).unwrap_err();
    assert_eq!(
        error.to_string(),
        "store error: memory retention operation failed"
    );
    store
        .with_connection(|connection| {
            let state: (i64, i64, i64, i64) = connection
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM relationship_memories), (SELECT COUNT(*) FROM relationship_memory_retention_runs), (SELECT COUNT(*) FROM relationship_memory_retention_batches), (SELECT clock_watermark FROM relationship_memory_retention_state)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(store_error)?;
            assert_eq!(state, (1, 0, 0, before));
            Ok(())
        })
        .unwrap();
}

#[test]
fn worker_facing_memory_contract_has_no_maintenance_operation() {
    let source = include_str!("memory.rs");
    let trait_body = source
        .split("pub trait MemoryStore")
        .nth(1)
        .unwrap()
        .split("/// Optional narrowing filter")
        .next()
        .unwrap();
    assert!(!trait_body.contains("purge"));
    assert!(!trait_body.contains("retention"));
}

#[test]
fn retention_policy_rejects_unsafe_bounds() {
    assert!(RelationshipMemoryRetentionPolicy::new(1, 1, 1, 0, 0, 1_000).is_ok());
    for invalid in [
        RelationshipMemoryRetentionPolicy::new(0, 1, 1, 0, 0, 1),
        RelationshipMemoryRetentionPolicy::new(1, 2, 1, 0, 0, 1),
        RelationshipMemoryRetentionPolicy::new(1, 1, 1, 366, 0, 1),
        RelationshipMemoryRetentionPolicy::new(1, 1, 1, 0, 1_826, 1),
        RelationshipMemoryRetentionPolicy::new(1, 1, 1, 0, 0, 1_001),
    ] {
        assert!(matches!(invalid, Err(MemoryStoreError::InvalidInput)));
    }
}
