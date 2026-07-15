use std::collections::HashMap;

use super::*;
use crate::tools::memory::{MemoryKind, MemoryReference};
use sylvander_protocol::types::SessionId;

fn session(user: &str, agent: &str) -> SessionContext {
    SessionContext::new(
        UserId::new(user),
        AgentId::new(agent),
        SessionId::new("session"),
    )
}

#[tokio::test]
async fn roundtrips_restarts_and_isolates_relationships() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let alice = session("alice", "agent-a");
    let bob = session("bob", "agent-a");
    let mut entry = MemoryEntry::new("same", "Rust workspace", alice.clone())
        .with_kind(MemoryKind::ProjectFact)
        .with_tag("architecture")
        .with_importance(Importance::Critical)
        .with_reference(MemoryReference::File {
            path: "Cargo.toml".into(),
        });
    entry.created_at = 42;
    entry.last_accessed = Some(51);
    entry.access_count = 7;
    entry.metadata = HashMap::from([("source".into(), "user".into())]);

    let store = SqliteMemoryStore::open(file.path()).unwrap();
    store.store(&alice, entry.clone()).await.unwrap();
    store
        .store(&bob, MemoryEntry::new("same", "Bob value", bob.clone()))
        .await
        .unwrap();
    drop(store);

    let reopened = SqliteMemoryStore::open(file.path()).unwrap();
    assert_eq!(reopened.get(&alice, "same").await.unwrap(), Some(entry));
    assert_eq!(
        reopened.get(&bob, "same").await.unwrap().unwrap().content,
        "Bob value"
    );
    assert!(
        reopened
            .get(&session("alice", "agent-b"), "same")
            .await
            .unwrap()
            .is_none()
    );
    reopened.delete(&alice, "same").await.unwrap();
    drop(reopened);
    assert!(
        SqliteMemoryStore::open(file.path())
            .unwrap()
            .get(&alice, "same")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn search_is_stable_bounded_and_future_schema_fails_closed() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let ctx = session("alice", "agent-a");
    let store = SqliteMemoryStore::open(file.path()).unwrap();
    for (id, importance) in [
        ("low", Importance::Low),
        ("z", Importance::High),
        ("a", Importance::High),
    ] {
        let mut entry = MemoryEntry::new(id, "Needle", ctx.clone()).with_importance(importance);
        entry.created_at = 10;
        store.store(&ctx, entry).await.unwrap();
    }
    let found = store
        .search(
            &ctx,
            "needle",
            MemoryFilter {
                limit: Some(2),
                ..MemoryFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        found
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    drop(store);

    let connection = Connection::open(file.path()).unwrap();
    connection
        .execute(
            "UPDATE memory_schema_migrations SET version = 2 WHERE component = ?1",
            [COMPONENT],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        SqliteMemoryStore::open(file.path()),
        Err(MemoryStoreError::Store(_))
    ));
}

#[test]
fn opens_in_memory_with_current_schema() {
    SqliteMemoryStore::open_in_memory().unwrap();
}
