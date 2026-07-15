use super::*;
use crate::tools::memory::{
    Importance, MemoryActorKind, MemoryAppend, MemoryExecutionContext, MemoryFilter, MemoryKind,
    MemoryReference,
};
use sylvander_protocol::SessionContext;

fn worker(user: &str, agent: &str) -> MemoryExecutionContext {
    MemoryExecutionContext::worker(&SessionContext::new(user, agent, "session"))
}

#[tokio::test]
async fn roundtrips_restarts_and_isolates_relationships() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let alice = worker("alice", "agent-a");
    let bob = worker("bob", "agent-a");
    let other_agent = worker("alice", "agent-b");
    let mut append = MemoryAppend::new("Rust workspace")
        .with_kind(MemoryKind::ProjectFact)
        .with_tag("architecture")
        .with_importance(Importance::Critical)
        .with_reference(MemoryReference::File {
            path: "Cargo.toml".into(),
        });
    append.metadata.insert("source".into(), "user".into());

    let store = SqliteMemoryStore::open(file.path()).unwrap();
    let entry = store.append_relationship(&alice, append).await.unwrap();
    let bob_entry = store
        .append_relationship(&bob, MemoryAppend::new("Bob value"))
        .await
        .unwrap();
    drop(store);

    let reopened = SqliteMemoryStore::open(file.path()).unwrap();
    assert_eq!(
        reopened.get_relationship(&alice, &entry.id).await.unwrap(),
        Some(entry.clone())
    );
    assert_eq!(
        reopened
            .get_relationship(&bob, &bob_entry.id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "Bob value"
    );
    assert!(
        reopened
            .get_relationship(&other_agent, &entry.id)
            .await
            .unwrap()
            .is_none()
    );

    let foreign = reopened
        .delete_relationship(&bob, &entry.id, entry.revision)
        .await
        .unwrap_err();
    let missing = reopened
        .delete_relationship(&bob, "00000000-0000-0000-0000-000000000000", entry.revision)
        .await
        .unwrap_err();
    assert_eq!(foreign.to_string(), missing.to_string());
    assert!(
        reopened
            .get_relationship(&alice, &entry.id)
            .await
            .unwrap()
            .is_some()
    );
    reopened
        .delete_relationship(&alice, &entry.id, entry.revision)
        .await
        .unwrap();
    drop(reopened);
    assert!(
        SqliteMemoryStore::open(file.path())
            .unwrap()
            .get_relationship(&alice, &entry.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn search_is_bounded_and_unknown_schema_fails_closed() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let ctx = worker("alice", "agent-a");
    let store = SqliteMemoryStore::open(file.path()).unwrap();
    for importance in [Importance::Low, Importance::High, Importance::High] {
        store
            .append_relationship(
                &ctx,
                MemoryAppend::new("Needle").with_importance(importance),
            )
            .await
            .unwrap();
    }
    let found = store
        .search_relationship(
            &ctx,
            "needle",
            MemoryFilter {
                limit: Some(2),
                ..MemoryFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
    assert!(
        found
            .iter()
            .all(|entry| entry.importance == Importance::High)
    );
    drop(store);

    let connection = Connection::open(file.path()).unwrap();
    connection
        .execute(
            "UPDATE memory_schema_migrations SET version = 1 WHERE component = ?1",
            [COMPONENT],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        SqliteMemoryStore::open(file.path()),
        Err(MemoryStoreError::Store(_))
    ));
}

#[tokio::test]
async fn guardian_and_system_relationship_access_fails_closed() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    for actor in [MemoryActorKind::Guardian, MemoryActorKind::SystemService] {
        let ctx = MemoryExecutionContext::privileged_for_test(actor);
        assert!(matches!(
            store
                .append_relationship(&ctx, MemoryAppend::new("forbidden"))
                .await,
            Err(MemoryStoreError::AccessDenied)
        ));
        assert!(matches!(
            store
                .search_relationship(&ctx, "", MemoryFilter::default())
                .await,
            Err(MemoryStoreError::AccessDenied)
        ));
    }
}

#[test]
fn opens_in_memory_with_current_schema() {
    SqliteMemoryStore::open_in_memory().unwrap();
}
