use super::*;
use crate::tools::memory::{
    Importance, MemoryActorKind, MemoryAppend, MemoryExecutionContext, MemoryExpiryPatch,
    MemoryFilter, MemoryPatch, MemoryReference,
};
use sylvander_protocol::SessionContext;

fn worker() -> MemoryExecutionContext {
    MemoryExecutionContext::worker(&SessionContext::new("alice", "agent-a", "session"))
}

#[tokio::test]
async fn audit_is_content_safe_append_only_and_cas_consistent() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    let raw_trace = format!("SECRET-trace\n\0{}", "x".repeat(128 * 1024));
    let ctx = MemoryExecutionContext::worker(
        &SessionContext::new("alice", "agent-a", "session").with_trace_id(&raw_trace),
    );
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
    let trace = entry.provenance.trace_id.as_deref().unwrap();
    assert_eq!(trace.len(), 71);
    assert!(trace.starts_with("sha256:"));
    assert!(trace[7..].bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(!trace.contains("SECRET-trace"));
    assert!(!trace.chars().any(char::is_control));

    store
        .with_connection(|connection| {
            let stored_trace: String = connection
                .query_row(
                    "SELECT origin_trace_id FROM relationship_memories WHERE id = ?1",
                    [&entry.id],
                    |row| row.get(0),
                )
                .map_err(store_error)?;
            let audit_trace: String = connection
                .query_row(
                    "SELECT trace_id FROM relationship_memory_audit WHERE operation = 'append'",
                    [],
                    |row| row.get(0),
                )
                .map_err(store_error)?;
            assert_eq!(stored_trace, trace);
            assert_eq!(audit_trace, trace);
            assert!(!stored_trace.contains(&raw_trace));
            assert!(!audit_trace.contains(&raw_trace));
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

#[tokio::test]
async fn update_and_supersede_are_cas_guarded_and_audited() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    let ctx = worker();
    let original = store
        .append_relationship(&ctx, MemoryAppend::new("old").with_ttl(60))
        .await
        .unwrap();
    let patch = MemoryPatch {
        content: Some("updated".into()),
        importance: Some(Importance::Critical),
        expiry: Some(MemoryExpiryPatch::AfterSeconds(30)),
        ..MemoryPatch::default()
    };
    assert!(matches!(
        store
            .update_relationship(&ctx, &original.id, 2, patch.clone())
            .await,
        Err(MemoryStoreError::Conflict)
    ));
    let updated = store
        .update_relationship(&ctx, &original.id, 1, patch)
        .await
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.content, "updated");
    assert_eq!(updated.importance, Importance::Critical);
    assert!(updated.expires_at.is_some());
    assert_eq!(updated.provenance, original.provenance);

    assert!(matches!(
        store
            .supersede_relationship(
                &ctx,
                &original.id,
                original.revision,
                MemoryAppend::new("stale"),
            )
            .await,
        Err(MemoryStoreError::Conflict)
    ));
    let replacement = store
        .supersede_relationship(
            &ctx,
            &original.id,
            updated.revision,
            MemoryAppend::new("replacement"),
        )
        .await
        .unwrap();
    assert!(
        store
            .get_relationship(&ctx, &original.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .search_relationship(&ctx, "", MemoryFilter::default())
            .await
            .unwrap(),
        [replacement]
    );
    assert!(matches!(
        store
            .delete_relationship(&ctx, &original.id, updated.revision + 1)
            .await,
        Err(MemoryStoreError::NotFound)
    ));
    store
        .with_connection(|connection| {
            let invariant: (i64, i64, i64) = connection
                .query_row(
                    "SELECT old.revision, replacement.owner_user = old.owner_user AND replacement.owner_agent = old.owner_agent, replacement.record_key <> old.record_key FROM relationship_memories old JOIN relationship_memories replacement ON replacement.record_key = old.superseded_by_record_key WHERE old.id = ?1",
                    [&original.id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(store_error)?;
            assert_eq!(invariant, (3, 1, 1));
            let operations: String = connection
                .query_row(
                    "SELECT group_concat(operation, ',') FROM (SELECT operation FROM relationship_memory_audit ORDER BY sequence)",
                    [],
                    |row| row.get(0),
                )
                .map_err(store_error)?;
            assert_eq!(operations, "append,update,append,supersede");
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn audit_failure_rolls_back_update_and_supersede() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    let ctx = worker();
    let original = store
        .append_relationship(&ctx, MemoryAppend::new("unchanged"))
        .await
        .unwrap();
    store
        .with_connection(|connection| {
            connection
                .execute_batch(
                    "CREATE TRIGGER reject_mutation_audit BEFORE INSERT ON relationship_memory_audit WHEN NEW.operation IN ('update', 'supersede') BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;",
                )
                .map_err(store_error)
        })
        .unwrap();
    let update_error = store
        .update_relationship(
            &ctx,
            &original.id,
            original.revision,
            MemoryPatch {
                content: Some("must rollback".into()),
                ..MemoryPatch::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        update_error.to_string(),
        "store error: memory mutation failed"
    );
    let supersede_error = store
        .supersede_relationship(
            &ctx,
            &original.id,
            original.revision,
            MemoryAppend::new("must also rollback"),
        )
        .await
        .unwrap_err();
    assert_eq!(
        supersede_error.to_string(),
        "store error: memory mutation failed"
    );
    store
        .with_connection(|connection| {
            let state: (i64, String, i64, i64) = connection
                .query_row(
                    "SELECT COUNT(*), MIN(content), MIN(revision), (SELECT COUNT(*) FROM relationship_memory_audit) FROM relationship_memories",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(store_error)?;
            assert_eq!(state, (1, "unchanged".into(), 1, 1));
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn inactive_mutations_are_indistinguishable_from_missing() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    let ctx = worker();
    let expired = store
        .append_relationship(&ctx, MemoryAppend::new("expired"))
        .await
        .unwrap();
    store
        .with_connection(|connection| {
            connection
                .execute(
                    "UPDATE relationship_memories SET expires_at = ?1 WHERE id = ?2",
                    params![crate::session::now_secs(), expired.id],
                )
                .map_err(store_error)?;
            Ok(())
        })
        .unwrap();
    let missing = store
        .delete_relationship(&ctx, "missing", 1)
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        store
            .delete_relationship(&ctx, &expired.id, expired.revision)
            .await
            .unwrap_err()
            .to_string(),
        missing
    );
    assert_eq!(
        store
            .update_relationship(
                &ctx,
                &expired.id,
                expired.revision,
                MemoryPatch {
                    content: Some("hidden".into()),
                    ..MemoryPatch::default()
                },
            )
            .await
            .unwrap_err()
            .to_string(),
        missing
    );
    assert_eq!(
        store
            .supersede_relationship(
                &ctx,
                &expired.id,
                expired.revision,
                MemoryAppend::new("hidden"),
            )
            .await
            .unwrap_err()
            .to_string(),
        missing
    );
}
