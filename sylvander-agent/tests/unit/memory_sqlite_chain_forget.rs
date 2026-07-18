use super::*;
use crate::tools::memory::{MemoryAppend, MemoryExecutionContext, MemoryOwner};
use sylvander_protocol::SessionContext;

fn worker(user: &str, agent: &str) -> MemoryExecutionContext {
    MemoryExecutionContext::application_worker(&SessionContext::new(user, agent, "session"))
}

fn relationship_owner(user: &str, agent: &str) -> MemoryOwner {
    MemoryOwner::Relationship {
        user_id: UserId::new(user),
        agent_id: AgentId::new(agent),
    }
}

#[tokio::test]
async fn maintenance_forgets_the_complete_chain_with_content_safe_audit() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    let ctx = worker("alice", "agent-a");
    let sentinel = "SECRET-chain-content";
    let first = store
        .append_relationship(&ctx, MemoryAppend::new(sentinel))
        .await
        .unwrap();
    let second = store
        .supersede_relationship(&ctx, &first.id, first.revision, MemoryAppend::new("second"))
        .await
        .unwrap();
    let current = store
        .supersede_relationship(
            &ctx,
            &second.id,
            second.revision,
            MemoryAppend::new("current"),
        )
        .await
        .unwrap();

    assert!(matches!(
        store
            .delete_relationship(&ctx, &current.id, current.revision)
            .await,
        Err(MemoryStoreError::Conflict)
    ));
    let report = store
        .maintenance()
        .forget_supersession_chain(
            &relationship_owner("alice", "agent-a"),
            &current.id,
            current.revision,
        )
        .unwrap();
    assert_eq!(report.deleted_count, 3);

    store
        .with_connection(|connection| {
            let state: (i64, i64, String) = connection
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM relationship_memories), (SELECT COUNT(*) FROM relationship_memory_audit WHERE operation = 'forget_chain'), (SELECT group_concat(event_id || operation || target_record_key || actor_kind || COALESCE(actor_user_id, '') || COALESCE(actor_agent_id, '') || COALESCE(session_id, '') || COALESCE(trace_id, ''), '') FROM relationship_memory_audit)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(store_error)?;
            assert_eq!((state.0, state.1), (0, 3));
            assert!(!state.2.contains(sentinel));
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn chain_forget_is_cas_guarded_and_audit_failure_rolls_back_every_row() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    let ctx = worker("alice", "agent-a");
    let first = store
        .append_relationship(&ctx, MemoryAppend::new("first"))
        .await
        .unwrap();
    let current = store
        .supersede_relationship(
            &ctx,
            &first.id,
            first.revision,
            MemoryAppend::new("current"),
        )
        .await
        .unwrap();
    let maintenance = store.maintenance();
    assert!(matches!(
        maintenance.forget_supersession_chain(
            &relationship_owner("alice", "agent-a"),
            &current.id,
            current.revision + 1,
        ),
        Err(MemoryStoreError::Conflict)
    ));

    store
        .with_connection(|connection| {
            connection
                .execute_batch(
                    "CREATE TRIGGER reject_chain_forget_audit BEFORE INSERT ON relationship_memory_audit WHEN NEW.operation = 'forget_chain' BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
                )
                .map_err(store_error)
        })
        .unwrap();
    assert_eq!(
        maintenance
            .forget_supersession_chain(
                &relationship_owner("alice", "agent-a"),
                &current.id,
                current.revision,
            )
            .unwrap_err()
            .to_string(),
        "store error: memory chain forget failed"
    );
    store
        .with_connection(|connection| {
            let state: (i64, i64) = connection
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM relationship_memories), (SELECT COUNT(*) FROM relationship_memory_audit WHERE operation = 'forget_chain')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(store_error)?;
            assert_eq!(state, (2, 0));
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn malformed_cross_owner_inbound_link_fails_closed() {
    let store = SqliteMemoryStore::open_in_memory().unwrap();
    let alice = worker("alice", "agent-a");
    let current = store
        .append_relationship(&alice, MemoryAppend::new("alice-current"))
        .await
        .unwrap();
    let mallory = worker("mallory", "agent-a");
    let foreign = store
        .append_relationship(&mallory, MemoryAppend::new("foreign"))
        .await
        .unwrap();
    store
        .with_connection(|connection| {
            connection
                .execute(
                    "UPDATE relationship_memories SET superseded_by_record_key = (SELECT record_key FROM relationship_memories WHERE id = ?1) WHERE id = ?2",
                    params![current.id, foreign.id],
                )
                .map_err(store_error)?;
            Ok(())
        })
        .unwrap();

    assert!(matches!(
        store.maintenance().forget_supersession_chain(
            &relationship_owner("alice", "agent-a"),
            &current.id,
            current.revision,
        ),
        Err(MemoryStoreError::Conflict)
    ));
    store
        .with_connection(|connection| {
            let state: (i64, i64) = connection
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM relationship_memories), (SELECT COUNT(*) FROM relationship_memory_audit WHERE operation = 'forget_chain')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(store_error)?;
            assert_eq!(state, (2, 0));
            Ok(())
        })
        .unwrap();
}

#[test]
fn model_facing_memory_contract_has_no_chain_forget_operation() {
    let source = include_str!("../../src/tools/memory.rs");
    let trait_body = source
        .split("pub trait MemoryStore")
        .nth(1)
        .unwrap()
        .split("/// Optional narrowing filter")
        .next()
        .unwrap();
    assert!(!trait_body.contains("forget"));
    assert!(!trait_body.contains("maintenance"));
}
