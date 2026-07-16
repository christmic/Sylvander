use super::*;
use crate::tools::memory::{MemoryAppend, MemoryExecutionContext, MemoryStore};
use sylvander_protocol::SessionContext;

const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

fn policy(revision: u64, ttl_days: u32) -> RelationshipMemoryRetentionPolicy {
    RelationshipMemoryRetentionPolicy::new(revision, ttl_days, ttl_days, 1, 7, 10).unwrap()
}

fn open(
    database: &Path,
    anchor: &Path,
    policy: RelationshipMemoryRetentionPolicy,
) -> SqliteMemoryStore {
    SqliteMemoryStore::open_with_integrity(
        database,
        policy,
        MemoryIntegrityConfig::new(anchor, KEY).unwrap(),
    )
    .unwrap()
}

fn worker() -> MemoryExecutionContext {
    MemoryExecutionContext::application_worker(&SessionContext::new(
        "alice",
        "agent-a",
        "session-a",
    ))
}

#[tokio::test]
async fn first_policy_is_unavailable_until_activation_and_activation_is_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let anchor = directory.path().join("anchor.json");
    let store = open(&database, &anchor, policy(1, 30));
    let maintenance = store.maintenance();

    assert!(!maintenance.has_active_retention_policy().unwrap());
    assert!(
        store
            .append_relationship(&worker(), MemoryAppend::new("before-ready"))
            .await
            .is_err()
    );

    maintenance.activate_staged_retention_policy().unwrap();
    maintenance.activate_staged_retention_policy().unwrap();
    assert!(maintenance.has_active_retention_policy().unwrap());
    let entry = store
        .append_relationship(&worker(), MemoryAppend::new("after-ready"))
        .await
        .unwrap();
    assert_eq!(entry.retention_policy_revision, 1);

    drop(store);
    let reopened = open(&database, &anchor, policy(1, 30));
    assert!(
        reopened
            .maintenance()
            .has_active_retention_policy()
            .unwrap()
    );
    reopened
        .maintenance()
        .activate_staged_retention_policy()
        .unwrap();
}

#[test]
fn abandoned_rollout_does_not_lock_out_the_active_or_retried_revision() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let anchor = directory.path().join("anchor.json");
    let initial = open(&database, &anchor, policy(1, 30));
    initial
        .maintenance()
        .activate_staged_retention_policy()
        .unwrap();
    drop(initial);

    // Simulate a Runtime that stages revision 2 and then fails a later startup
    // readiness check. The staged row survives the crash, but is not active.
    drop(open(&database, &anchor, policy(2, 60)));

    let rollback = open(&database, &anchor, policy(1, 30));
    rollback
        .maintenance()
        .activate_staged_retention_policy()
        .unwrap();
    drop(rollback);

    let retry = open(&database, &anchor, policy(2, 60));
    retry
        .maintenance()
        .activate_staged_retention_policy()
        .unwrap();
    drop(retry);

    assert!(
        SqliteMemoryStore::open_with_integrity(
            &database,
            policy(1, 30),
            MemoryIntegrityConfig::new(&anchor, KEY).unwrap(),
        )
        .is_err()
    );
}

#[test]
fn concurrent_different_rollouts_require_the_winning_stage_cas() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let anchor = directory.path().join("anchor.json");
    let initial = open(&database, &anchor, policy(1, 30));
    initial
        .maintenance()
        .activate_staged_retention_policy()
        .unwrap();
    drop(initial);

    let revision_two = open(&database, &anchor, policy(2, 60));
    let revision_three = open(&database, &anchor, policy(3, 90));
    assert!(matches!(
        revision_two
            .maintenance()
            .activate_staged_retention_policy(),
        Err(MemoryStoreError::Conflict)
    ));
    revision_three
        .maintenance()
        .activate_staged_retention_policy()
        .unwrap();
}

#[test]
fn concurrent_identical_rollouts_activate_idempotently() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let anchor = directory.path().join("anchor.json");
    let initial = open(&database, &anchor, policy(1, 30));
    initial
        .maintenance()
        .activate_staged_retention_policy()
        .unwrap();
    drop(initial);

    let first = open(&database, &anchor, policy(2, 60));
    let second = open(&database, &anchor, policy(2, 60));
    first
        .maintenance()
        .activate_staged_retention_policy()
        .unwrap();
    second
        .maintenance()
        .activate_staged_retention_policy()
        .unwrap();
}
