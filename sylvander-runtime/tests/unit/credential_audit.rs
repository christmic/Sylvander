use std::fs;

use rusqlite::Connection;
use tempfile::tempdir;

use super::*;

#[tokio::test]
async fn events_survive_restart_and_queries_are_subject_isolated() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("credential-operations.db");
    let alpha = CredentialAuditSubject::channel_instance("telegram-alpha").unwrap();
    let beta = CredentialAuditSubject::channel_instance("telegram-beta").unwrap();
    let ledger = CredentialOperationAuditLedger::open(&path).await.unwrap();
    ledger
        .record_at(
            &alpha,
            CredentialAuditOperation::Create,
            Some(1),
            CredentialAuditResult::Succeeded,
            100,
        )
        .await
        .unwrap();
    ledger
        .record_at(
            &beta,
            CredentialAuditOperation::Failure,
            None,
            CredentialAuditResult::Unavailable,
            101,
        )
        .await
        .unwrap();
    drop(ledger);

    let reopened = CredentialOperationAuditLedger::open(&path).await.unwrap();
    let alpha_events = reopened.list(&alpha, 10).await.unwrap();
    let beta_events = reopened.list(&beta, 10).await.unwrap();

    assert_eq!(alpha_events.len(), 1);
    assert_eq!(alpha_events[0].operation, CredentialAuditOperation::Create);
    assert_eq!(alpha_events[0].credential_revision, Some(1));
    assert_eq!(alpha_events[0].result, CredentialAuditResult::Succeeded);
    assert_eq!(beta_events.len(), 1);
    assert_eq!(beta_events[0].operation, CredentialAuditOperation::Failure);
    assert_eq!(beta_events[0].result, CredentialAuditResult::Unavailable);
}

#[tokio::test]
async fn provider_binding_is_persisted_only_as_a_digest() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("credential-operations.db");
    let binding = "provider:anthropic:never-store-this-reference";
    let subject = CredentialAuditSubject::provider("anthropic", binding).unwrap();
    let ledger = CredentialOperationAuditLedger::open(&path).await.unwrap();
    ledger
        .record_at(
            &subject,
            CredentialAuditOperation::Renew,
            Some(7),
            CredentialAuditResult::Succeeded,
            200,
        )
        .await
        .unwrap();
    drop(ledger);

    let bytes = fs::read(path).unwrap();
    assert!(
        !bytes
            .windows(binding.len())
            .any(|window| window == binding.as_bytes())
    );
}

#[tokio::test]
async fn retention_is_finite_and_deletes_in_bounded_batches() {
    let ledger = CredentialOperationAuditLedger::open_in_memory_with_policy(50, 2)
        .await
        .unwrap();
    let subject = CredentialAuditSubject::provider("anthropic", "provider:key").unwrap();
    for revision in 1..=3 {
        ledger
            .record_at(
                &subject,
                CredentialAuditOperation::Renew,
                Some(revision),
                CredentialAuditResult::Succeeded,
                100,
            )
            .await
            .unwrap();
    }
    ledger
        .record_at(
            &subject,
            CredentialAuditOperation::Rotate,
            Some(4),
            CredentialAuditResult::Succeeded,
            200,
        )
        .await
        .unwrap();
    let first_pass = ledger.list(&subject, 10).await.unwrap();
    assert_eq!(first_pass.len(), 2);
    assert!(
        first_pass
            .iter()
            .any(|event| event.credential_revision == Some(4))
    );

    ledger
        .record_at(
            &subject,
            CredentialAuditOperation::Renew,
            Some(4),
            CredentialAuditResult::Succeeded,
            201,
        )
        .await
        .unwrap();
    let second_pass = ledger.list(&subject, 10).await.unwrap();
    assert_eq!(second_pass.len(), 2);
    assert!(
        second_pass
            .iter()
            .all(|event| event.occurred_at_unix_secs >= 200)
    );
}

#[tokio::test]
async fn old_future_partial_and_foreign_schemas_fail_closed() {
    let directory = tempdir().unwrap();
    for (name, sql) in [
        (
            "old.db",
            "PRAGMA application_id=1398350657; PRAGMA user_version=0; CREATE TABLE credential_operation_audit(event_id TEXT PRIMARY KEY)",
        ),
        (
            "future.db",
            "PRAGMA application_id=1398350657; PRAGMA user_version=2; CREATE TABLE credential_operation_audit(event_id TEXT PRIMARY KEY)",
        ),
        ("foreign.db", "CREATE TABLE unrelated(secret TEXT)"),
    ] {
        let path = directory.path().join(name);
        Connection::open(&path).unwrap().execute_batch(sql).unwrap();
        let result = CredentialOperationAuditLedger::open(&path).await;
        assert!(matches!(result, Err(CredentialAuditError::Schema)));
    }

    let current = directory.path().join("partial.db");
    let ledger = CredentialOperationAuditLedger::open(&current)
        .await
        .unwrap();
    drop(ledger);
    Connection::open(&current)
        .unwrap()
        .execute_batch("DROP INDEX credential_operation_audit_subject_time")
        .unwrap();
    let result = CredentialOperationAuditLedger::open(&current).await;
    assert!(matches!(result, Err(CredentialAuditError::Schema)));
}

#[test]
fn identity_and_query_bounds_reject_ambiguous_values() {
    assert!(matches!(
        CredentialAuditSubject::channel_instance(" bad"),
        Err(CredentialAuditError::InvalidInput)
    ));
    assert!(matches!(
        CredentialAuditSubject::provider("anthropic", "bad\nreference"),
        Err(CredentialAuditError::InvalidInput)
    ));
}
