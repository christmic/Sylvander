use super::*;
use crate::tools::memory::{
    MemoryActorKind, MemoryAppend, MemoryExecutionContext, MemoryFilter, MemoryReference,
};
use sylvander_protocol::SessionContext;

fn worker() -> MemoryExecutionContext {
    MemoryExecutionContext::worker(&SessionContext::new("alice", "agent-a", "session"))
}

#[tokio::test]
async fn audit_is_content_safe_append_only_and_cas_consistent() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    let ctx = worker();
    let sentinel = "SECRET-memory-payload";
    let mut append = MemoryAppend::new(sentinel)
        .with_tag(sentinel)
        .with_ttl(60)
        .with_reference(MemoryReference::Url {
            url: sentinel.into(),
        });
    append.metadata.insert("note".into(), sentinel.into());
    let entry = store.append_relationship(&ctx, append).await.unwrap();
    assert_eq!(entry.revision, 1);
    assert_eq!(entry.updated_at, entry.created_at);
    assert!(
        entry
            .expires_at
            .is_some_and(|expiry| expiry > entry.created_at)
    );
    assert_eq!(entry.provenance.actor, MemoryActorKind::Worker);
    assert!(entry.provenance.trusted);

    store
        .with_connection(|connection| {
            let audit: String = connection
                .query_row(
                    "SELECT group_concat(event_id || operation || target_record_key || actor_kind || COALESCE(actor_user_id, '') || COALESCE(actor_agent_id, '') || COALESCE(session_id, '') || COALESCE(trace_id, '')) FROM relationship_memory_audit",
                    [],
                    |row| row.get(0),
                )
                .map_err(store_error)?;
            assert!(!audit.contains(sentinel));
            assert!(
                connection
                    .execute(
                        "UPDATE relationship_memory_audit SET operation = 'tampered'",
                        [],
                    )
                    .is_err()
            );
            assert!(
                connection
                    .execute("DELETE FROM relationship_memory_audit", [])
                    .is_err()
            );
            Ok(())
        })
        .unwrap();

    assert!(matches!(
        store
            .delete_relationship(&ctx, &entry.id, entry.revision + 1)
            .await,
        Err(MemoryStoreError::Conflict)
    ));
    store
        .delete_relationship(&ctx, &entry.id, entry.revision)
        .await
        .unwrap();
    let operations = store
        .with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT operation FROM relationship_memory_audit ORDER BY sequence")
                .map_err(store_error)?;
            statement
                .query_map([], |row| row.get(0))
                .map_err(store_error)?
                .collect::<Result<Vec<String>, _>>()
                .map_err(store_error)
        })
        .unwrap();
    assert_eq!(operations, ["append", "delete"]);
}

#[test]
fn unmanaged_or_damaged_schema_fails_closed_without_repair() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let connection = Connection::open(file.path()).unwrap();
    connection
        .execute_batch("CREATE TABLE relationship_memories (legacy TEXT)")
        .unwrap();
    drop(connection);
    assert!(matches!(
        SqliteMemoryStore::open(file.path()),
        Err(MemoryStoreError::Store(_))
    ));
    let connection = Connection::open(file.path()).unwrap();
    let ledger_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memory_schema_migrations')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!ledger_exists);

    let damaged = tempfile::NamedTempFile::new().unwrap();
    drop(SqliteMemoryStore::open(damaged.path()).unwrap());
    let connection = Connection::open(damaged.path()).unwrap();
    connection
        .execute_batch("DROP TRIGGER relationship_memory_audit_no_delete")
        .unwrap();
    drop(connection);
    assert!(matches!(
        SqliteMemoryStore::open(damaged.path()),
        Err(MemoryStoreError::Store(_))
    ));
}

#[tokio::test]
async fn expired_rows_are_hidden_from_get_and_search() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    let ctx = worker();
    let entry = store
        .append_relationship(&ctx, MemoryAppend::new("expired"))
        .await
        .unwrap();
    store
        .with_connection(|connection| {
            connection
                .execute(
                    "UPDATE relationship_memories SET expires_at = ?1 WHERE id = ?2",
                    params![crate::session::now_secs(), entry.id],
                )
                .map_err(store_error)?;
            Ok(())
        })
        .unwrap();
    assert!(
        store
            .get_relationship(&ctx, &entry.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .search_relationship(&ctx, "expired", MemoryFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
}
