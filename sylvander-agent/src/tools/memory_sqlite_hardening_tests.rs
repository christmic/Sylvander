use super::*;
use crate::tools::memory::{
    MemoryActorKind, MemoryAppend, MemoryExecutionContext, MemoryFilter, MemoryPatch,
};
use sylvander_protocol::SessionContext;

const SENTINEL: &str = "SECRET-injected-SQL-relationship_memories";

fn worker() -> MemoryExecutionContext {
    MemoryExecutionContext::worker(&SessionContext::new("alice", "agent-a", "session"))
}

fn fresh_database() -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    drop(SqliteMemoryStore::open(file.path()).unwrap());
    file
}

fn assert_schema_rejected(file: &tempfile::NamedTempFile) {
    let error = SqliteMemoryStore::open(file.path()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "store error: unsupported relationship memory schema"
    );
    assert!(!format!("{error:?}").contains(SENTINEL));
}

fn assert_redacted(error: MemoryStoreError, expected: &str) {
    let display = error.to_string();
    let debug = format!("{error:?}");
    assert_eq!(display, expected);
    for secret in [SENTINEL, "relationship_memories", "injected", "sqlite"] {
        assert!(!display.contains(secret));
        assert!(!debug.contains(secret));
    }
}

#[test]
fn exact_schema_rejects_noop_trigger_weak_constraint_and_wrong_index_type() {
    let noop_trigger = fresh_database();
    let connection = Connection::open(noop_trigger.path()).unwrap();
    connection
        .execute_batch(
            "DROP TRIGGER relationship_memory_audit_no_update;
             CREATE TRIGGER relationship_memory_audit_no_update
             BEFORE UPDATE ON relationship_memory_audit BEGIN SELECT 1; END;",
        )
        .unwrap();
    drop(connection);
    assert_schema_rejected(&noop_trigger);

    let weak_constraint = fresh_database();
    let connection = Connection::open(weak_constraint.path()).unwrap();
    connection
        .execute_batch("PRAGMA writable_schema = ON")
        .unwrap();
    let changed = connection
        .execute(
            "UPDATE sqlite_master SET sql = replace(sql, 'revision INTEGER NOT NULL CHECK (revision >= 1)', 'revision INTEGER NOT NULL CHECK (revision >= 0)') WHERE type = 'table' AND name = 'relationship_memories'",
            [],
        )
        .unwrap();
    assert_eq!(changed, 1);
    connection
        .execute_batch("PRAGMA writable_schema = OFF")
        .unwrap();
    drop(connection);
    assert_schema_rejected(&weak_constraint);

    let wrong_index = fresh_database();
    let connection = Connection::open(wrong_index.path()).unwrap();
    connection
        .execute_batch(
            "DROP INDEX relationship_memories_search;
             CREATE VIEW relationship_memories_search AS
             SELECT id FROM relationship_memories;",
        )
        .unwrap();
    drop(connection);
    assert_schema_rejected(&wrong_index);
}

#[test]
fn exact_schema_rejects_unexpected_trigger_on_owned_table() {
    let file = fresh_database();
    let connection = Connection::open(file.path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER unexpected_memory_trigger
             AFTER INSERT ON relationship_memories BEGIN SELECT 1; END;",
        )
        .unwrap();
    drop(connection);
    assert_schema_rejected(&file);
}

#[test]
fn open_errors_are_fixed_and_path_free() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(SENTINEL).join("memory.db");
    assert_redacted(
        SqliteMemoryStore::open(path).unwrap_err(),
        "store error: memory store operation failed",
    );
}

#[tokio::test]
async fn corrupt_rows_do_not_escape_through_search_or_get_errors() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    let ctx = worker();
    let entry = store
        .append_relationship(&ctx, MemoryAppend::new("safe"))
        .await
        .unwrap();
    store
        .with_connection(|connection| {
            connection
                .execute(
                    "UPDATE relationship_memories SET kind_json = ?1 WHERE id = ?2",
                    params![SENTINEL, entry.id],
                )
                .map_err(store_error)?;
            Ok(())
        })
        .unwrap();
    assert_redacted(
        store
            .search_relationship(&ctx, "", MemoryFilter::default())
            .await
            .unwrap_err(),
        "search error: memory search failed",
    );
    assert_redacted(
        store.get_relationship(&ctx, &entry.id).await.unwrap_err(),
        "store error: memory store operation failed",
    );
}

#[tokio::test]
async fn write_delete_update_and_supersede_errors_are_fixed() {
    let ctx = worker();

    let append_store = SqliteMemoryStore::open_in_memory().unwrap();
    install_abort_trigger(&append_store, "BEFORE INSERT", None);
    assert_redacted(
        append_store
            .append_relationship(&ctx, MemoryAppend::new("append"))
            .await
            .unwrap_err(),
        "store error: memory store operation failed",
    );

    let delete_store = SqliteMemoryStore::open_in_memory().unwrap();
    let delete_entry = delete_store
        .append_relationship(&ctx, MemoryAppend::new("delete"))
        .await
        .unwrap();
    install_abort_trigger(&delete_store, "BEFORE DELETE", None);
    assert_redacted(
        delete_store
            .delete_relationship(&ctx, &delete_entry.id, delete_entry.revision)
            .await
            .unwrap_err(),
        "delete error: memory delete failed",
    );

    let update_store = SqliteMemoryStore::open_in_memory().unwrap();
    let update_entry = update_store
        .append_relationship(&ctx, MemoryAppend::new("update"))
        .await
        .unwrap();
    install_abort_trigger(&update_store, "BEFORE UPDATE", None);
    assert_redacted(
        update_store
            .update_relationship(
                &ctx,
                &update_entry.id,
                update_entry.revision,
                MemoryPatch {
                    content: Some("changed".into()),
                    ..MemoryPatch::default()
                },
            )
            .await
            .unwrap_err(),
        "store error: memory mutation failed",
    );

    let supersede_store = SqliteMemoryStore::open_in_memory().unwrap();
    let supersede_entry = supersede_store
        .append_relationship(&ctx, MemoryAppend::new("supersede"))
        .await
        .unwrap();
    install_abort_trigger(
        &supersede_store,
        "BEFORE INSERT",
        Some("WHEN NEW.content = 'replacement'"),
    );
    assert_redacted(
        supersede_store
            .supersede_relationship(
                &ctx,
                &supersede_entry.id,
                supersede_entry.revision,
                MemoryAppend::new("replacement"),
            )
            .await
            .unwrap_err(),
        "store error: memory mutation failed",
    );
}

fn install_abort_trigger(store: &SqliteMemoryStore, timing: &str, condition: Option<&str>) {
    store
        .with_connection(|connection| {
            connection
                .execute_batch(&format!(
                    "CREATE TRIGGER reject_operation {timing} ON relationship_memories {} BEGIN SELECT RAISE(ABORT, '{SENTINEL}'); END;",
                    condition.unwrap_or("")
                ))
                .map_err(store_error)
        })
        .unwrap();
}

#[tokio::test]
async fn authorization_precedes_invalid_input_for_every_relationship_api() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    let ctx = MemoryExecutionContext::privileged_for_test(MemoryActorKind::Guardian);
    assert!(matches!(
        store.append_relationship(&ctx, MemoryAppend::new("")).await,
        Err(MemoryStoreError::AccessDenied)
    ));
    assert!(matches!(
        store.get_relationship(&ctx, "").await,
        Err(MemoryStoreError::AccessDenied)
    ));
    assert!(matches!(
        store
            .search_relationship(
                &ctx,
                &"x".repeat(MAX_MEMORY_QUERY_BYTES + 1),
                MemoryFilter::default(),
            )
            .await,
        Err(MemoryStoreError::AccessDenied)
    ));
    assert!(matches!(
        store
            .update_relationship(&ctx, "", 0, MemoryPatch::default())
            .await,
        Err(MemoryStoreError::AccessDenied)
    ));
    assert!(matches!(
        store
            .supersede_relationship(&ctx, "", 0, MemoryAppend::new(""))
            .await,
        Err(MemoryStoreError::AccessDenied)
    ));
    assert!(matches!(
        store.delete_relationship(&ctx, "", 0).await,
        Err(MemoryStoreError::AccessDenied)
    ));
}
