use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use sylvander_protocol::SessionContext;

use super::*;
use crate::tools::memory::{MemoryAppend, MemoryExecutionContext, MemoryPatch};

#[derive(Debug)]
struct TestClock(AtomicI64);

impl TestClock {
    fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl MemoryClock for TestClock {
    fn now_secs(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn worker() -> MemoryExecutionContext {
    MemoryExecutionContext::application_worker(&SessionContext::new("alice", "agent-a", "clock"))
}

fn policy() -> RelationshipMemoryRetentionPolicy {
    RelationshipMemoryRetentionPolicy::new(1, 1, 1, 0, 0, 10).unwrap()
}

#[tokio::test]
async fn rollback_cannot_revive_an_expired_memory_across_restart() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let clock = Arc::new(TestClock::new(1_000_000));
    let store = SqliteMemoryStore::open_with_retention_policy_and_clock(
        file.path(),
        policy(),
        clock.clone(),
    )
    .unwrap();
    let ctx = worker();
    let entry = store
        .append_relationship(&ctx, MemoryAppend::new("short").with_ttl(10))
        .await
        .unwrap();

    clock.set(1_000_009);
    assert!(
        store
            .get_relationship(&ctx, &entry.id)
            .await
            .unwrap()
            .is_some()
    );
    clock.set(1_000_011);
    assert!(
        store
            .get_relationship(&ctx, &entry.id)
            .await
            .unwrap()
            .is_none()
    );
    clock.set(1_000_000);
    assert!(
        store
            .get_relationship(&ctx, &entry.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .update_relationship(&ctx, &entry.id, entry.revision, MemoryPatch::default())
            .await
            .is_err()
    );
    drop(store);

    let reopened =
        SqliteMemoryStore::open_with_retention_policy_and_clock(file.path(), policy(), clock)
            .unwrap();
    assert!(
        reopened
            .get_relationship(&ctx, &entry.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn dangerous_forward_jump_is_quarantined_until_explicit_confirmation() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let base = 2_000_000;
    let clock = Arc::new(TestClock::new(base));
    let store = SqliteMemoryStore::open_with_retention_policy_and_clock(
        file.path(),
        policy(),
        clock.clone(),
    )
    .unwrap();
    let ctx = worker();
    let entry = store
        .append_relationship(&ctx, MemoryAppend::new("do not purge").with_ttl(10))
        .await
        .unwrap();
    let future = base + 10 * 365 * 24 * 60 * 60;
    clock.set(future);

    assert!(
        store
            .get_relationship(&ctx, &entry.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(store.maintenance().purge().is_err());
    store
        .with_connection(|connection| {
            let state: (i64, Option<i64>, i64) = connection
                .query_row(
                    "SELECT clock_watermark, quarantined_forward_time, (SELECT COUNT(*) FROM relationship_memories) FROM relationship_memory_retention_state",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(store_error)?;
            assert_eq!(state, (base, Some(future), 1));
            Ok(())
        })
        .unwrap();
    drop(store);

    clock.set(base);
    let reopened =
        SqliteMemoryStore::open_with_retention_policy_and_clock(file.path(), policy(), clock)
            .unwrap();
    assert!(reopened.maintenance().purge().is_err());
    assert!(
        reopened
            .get_relationship(&ctx, &entry.id)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        reopened.maintenance().confirm_quarantined_clock().unwrap(),
        future
    );
    assert!(
        reopened
            .get_relationship(&ctx, &entry.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(reopened.maintenance().purge().unwrap().expired_count, 1);
}
