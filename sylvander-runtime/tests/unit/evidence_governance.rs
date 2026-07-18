use super::*;
use crate::evidence::EvidenceEvent;
use sylvander_agent::mcp_stdio::{McpResultArtifact, McpResultArtifactSink};

fn encryption(key_id: &str, byte: u8) -> EvidenceEncryption {
    EvidenceEncryption::from_secret(key_id, &[byte; 32]).expect("valid test key")
}

fn policy(tenant_id: &str, key_id: &str, byte: u8) -> EvidenceGovernance {
    EvidenceGovernance::new(tenant_id, 30, encryption(key_id, byte)).expect("valid policy")
}

fn record(
    scope: EvidenceScope,
    id: &str,
    kind: GovernedRecordKind,
    payload: &[u8],
    created_at: i64,
) -> GovernedRecordInput {
    GovernedRecordInput {
        id: id.into(),
        scope,
        kind,
        classification: EvidenceClassification::Confidential,
        source_ref: format!("source:{id}"),
        media_type: "application/json".into(),
        payload: payload.to_vec(),
        created_at,
    }
}

#[test]
fn structural_redaction_preserves_shape_and_removes_content_fields() {
    let value = serde_json::json!({
        "type": "chat",
        "payload": "private prompt",
        "metadata": {
            "model": "model-a",
            "authorization": "Bearer secret",
            "nested": [{"token": "abc"}, {"count": 2}]
        },
        "attachments": [{
            "name": "notes.txt",
            "content": {"text": "private attachment"}
        }]
    });

    let redacted = structured_redact(&value);

    assert_eq!(redacted["type"], "chat");
    assert_eq!(redacted["metadata"]["model"], "model-a");
    assert_eq!(redacted["metadata"]["nested"][1]["count"], 2);
    assert_eq!(redacted["payload"], "[REDACTED]");
    assert_eq!(redacted["metadata"]["authorization"], "[REDACTED]");
    assert_eq!(redacted["metadata"]["nested"][0]["token"], "[REDACTED]");
    assert_eq!(redacted["attachments"][0]["content"]["text"], "[REDACTED]");
    let serialized = redacted.to_string();
    for secret in [
        "private prompt",
        "Bearer secret",
        "abc",
        "private attachment",
    ] {
        assert!(!serialized.contains(secret));
    }
}

#[tokio::test]
async fn encrypted_records_survive_restart_and_export_is_audited() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("evidence.sqlite");
    let scope = EvidenceScope::new("tenant-a", "alice");
    let payload = br#"{"payload":"HIGHLY-PRIVATE-PROMPT","result":"ok"}"#;
    let now = unix_timestamp();
    let store = EvidenceStore::open_governed(&path, policy("tenant-a", "key-1", 7))
        .await
        .unwrap();
    store
        .put_governed_record(record(
            scope.clone(),
            "event-1",
            GovernedRecordKind::Event,
            payload,
            now,
        ))
        .await
        .unwrap();
    drop(store);

    let database = std::fs::read(&path).unwrap();
    assert!(
        !database
            .windows(b"HIGHLY-PRIVATE-PROMPT".len())
            .any(|window| window == b"HIGHLY-PRIVATE-PROMPT")
    );

    let reopened = EvidenceStore::open_governed(&path, policy("tenant-a", "key-1", 7))
        .await
        .unwrap();
    let export = reopened
        .export_governed_records(scope.clone(), vec!["event-1".into()], now + 1)
        .await
        .unwrap();
    assert_eq!(export.records.len(), 1);
    assert_eq!(export.records[0].payload, payload);
    assert_eq!(export.records[0].scope, scope);
    assert_eq!(export.digest_sha256.len(), 64);
    let audits = reopened
        .governance_audits(EvidenceScope::new("tenant-a", "alice"), 10)
        .await
        .unwrap();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].id, export.audit_id);
    assert_eq!(audits[0].action, "export");
    assert_eq!(audits[0].result_digest_sha256, export.digest_sha256);
}

#[tokio::test]
async fn database_binding_rejects_wrong_tenant_key_id_and_key_material() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("evidence.sqlite");
    drop(
        EvidenceStore::open_governed(&path, policy("tenant-a", "key-1", 7))
            .await
            .unwrap(),
    );

    let wrong_tenant = EvidenceStore::open_governed(&path, policy("tenant-b", "key-1", 7)).await;
    assert!(matches!(
        wrong_tenant,
        Err(EvidenceError::GovernanceBindingMismatch)
    ));
    let wrong_id = EvidenceStore::open_governed(&path, policy("tenant-a", "key-2", 7)).await;
    assert!(matches!(
        wrong_id,
        Err(EvidenceError::GovernanceBindingMismatch)
    ));
    let wrong_material = EvidenceStore::open_governed(&path, policy("tenant-a", "key-1", 8)).await;
    assert!(matches!(
        wrong_material,
        Err(EvidenceError::DecryptionFailed)
    ));
}

#[tokio::test]
async fn governance_store_rejects_non_current_schema_without_migration() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("evidence.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE evidence_governance_meta(
               singleton INTEGER PRIMARY KEY
             );",
        )
        .unwrap();
    drop(connection);

    let result = EvidenceStore::open_governed(&path, policy("tenant-a", "key-1", 7)).await;

    assert!(matches!(
        result,
        Err(EvidenceError::InvalidGovernanceSchema)
    ));
}

#[tokio::test]
async fn exact_scope_prevents_cross_tenant_and_cross_user_reads() {
    let store = EvidenceStore::open_governed_in_memory(policy("tenant-a", "key-1", 3))
        .await
        .unwrap();
    let alice = store.governed_scope("alice").unwrap();
    store
        .put_governed_record(record(
            alice.clone(),
            "event-1",
            GovernedRecordKind::Event,
            b"alice-content",
            10,
        ))
        .await
        .unwrap();

    let cross_tenant = store
        .export_governed_records(
            EvidenceScope::new("tenant-b", "alice"),
            vec!["event-1".into()],
            11,
        )
        .await;
    assert!(matches!(
        cross_tenant,
        Err(EvidenceError::EvidenceScopeMismatch)
    ));
    let cross_user = store
        .export_governed_records(
            EvidenceScope::new("tenant-a", "bob"),
            vec!["event-1".into()],
            11,
        )
        .await;
    assert!(matches!(
        cross_user,
        Err(EvidenceError::GovernedRecordNotFound)
    ));
    assert!(
        store
            .governance_audits(EvidenceScope::new("tenant-a", "alice"), 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn normalized_event_table_rejects_plaintext_content() {
    let store = EvidenceStore::open_governed_in_memory(policy("tenant-a", "key-1", 3))
        .await
        .unwrap();
    store
        .start_run("run-1".into(), "test".into(), 1)
        .await
        .unwrap();

    let result = store
        .append_event(EvidenceEvent {
            id: "event-1".into(),
            run_id: "run-1".into(),
            session_id: "session-1".into(),
            turn_id: None,
            event_type: "chat".into(),
            occurred_at: 2,
            observed_at: 2,
            payload_bytes: 6,
            payload_digest: None,
            payload_json: Some("secret".into()),
            privacy_class: "user_content".into(),
        })
        .await;

    assert!(matches!(
        result,
        Err(EvidenceError::PlaintextEvidenceRejected)
    ));
}

#[tokio::test]
async fn artifact_deletion_is_precise_irreversible_and_audited() {
    let store = EvidenceStore::open_governed_in_memory(policy("tenant-a", "key-1", 4))
        .await
        .unwrap();
    let scope = store.governed_scope("alice").unwrap();
    for (id, payload) in [
        ("artifact-1", b"GENERATED-SECRET-ONE".as_slice()),
        ("artifact-2", b"GENERATED-SECRET-TWO".as_slice()),
    ] {
        store
            .put_governed_record(record(
                scope.clone(),
                id,
                GovernedRecordKind::Artifact,
                payload,
                10,
            ))
            .await
            .unwrap();
    }

    let audit = store
        .delete_governed_records(
            scope.clone(),
            vec!["artifact-1".into()],
            "user_requested".into(),
            20,
        )
        .await
        .unwrap();
    assert_eq!(audit.action, "delete");
    assert_eq!(audit.record_count, 1);
    assert!(matches!(
        store
            .export_governed_records(scope.clone(), vec!["artifact-1".into()], 21)
            .await,
        Err(EvidenceError::GovernedRecordNotFound)
    ));
    let retained = store
        .export_governed_records(scope.clone(), vec!["artifact-2".into()], 21)
        .await
        .unwrap();
    assert_eq!(retained.records[0].payload, b"GENERATED-SECRET-TWO");
    assert!(matches!(
        store
            .put_governed_record(record(
                scope,
                "artifact-1",
                GovernedRecordKind::Artifact,
                b"replacement",
                22,
            ))
            .await,
        Err(EvidenceError::GovernedRecordDeleted)
    ));
}

#[tokio::test]
async fn mcp_generated_artifacts_use_the_governed_store() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("mcp-evidence.sqlite");
    let store = EvidenceStore::open_governed(&path, policy("tenant-a", "key-1", 6))
        .await
        .unwrap();
    let sink = EvidenceArtifactSink::new(store.clone()).unwrap();
    let locator = sink
        .persist(McpResultArtifact {
            user_id: "alice".into(),
            session_id: "session-1".into(),
            server: "filesystem".into(),
            operation: "read_file".into(),
            media_type: "application/json".into(),
            payload: br#"{"content":"PRIVATE-MCP-RESULT"}"#.to_vec(),
            created_at: unix_timestamp(),
        })
        .await
        .unwrap();
    let record_id = locator
        .strip_prefix("evidence-artifact:")
        .expect("opaque governed locator");
    let export = store
        .export_governed_records(
            EvidenceScope::new("tenant-a", "alice"),
            vec![record_id.into()],
            unix_timestamp(),
        )
        .await
        .unwrap();

    assert_eq!(export.records[0].kind, GovernedRecordKind::Artifact);
    assert_eq!(
        export.records[0].classification,
        EvidenceClassification::Restricted
    );
    assert!(
        export.records[0]
            .source_ref
            .starts_with("mcp:filesystem:read_file:session-sha256:")
    );
    assert_eq!(
        export.records[0].payload,
        br#"{"content":"PRIVATE-MCP-RESULT"}"#
    );
    drop(sink);
    drop(store);
    let database = std::fs::read(path).unwrap();
    assert!(
        !database
            .windows(b"PRIVATE-MCP-RESULT".len())
            .any(|window| window == b"PRIVATE-MCP-RESULT")
    );
}

#[tokio::test]
async fn finite_retention_applies_to_events_and_artifacts_after_restart() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("evidence.sqlite");
    let policy =
        || EvidenceGovernance::new("tenant-a", 1, encryption("key-1", 5)).expect("valid policy");
    let store = EvidenceStore::open_governed(&path, policy()).await.unwrap();
    let scope = store.governed_scope("alice").unwrap();
    let now = unix_timestamp();
    store
        .put_governed_record(record(
            scope.clone(),
            "event-old",
            GovernedRecordKind::Event,
            b"old-event",
            now - (2 * 86_400),
        ))
        .await
        .unwrap();
    store
        .put_governed_record(record(
            scope.clone(),
            "artifact-new",
            GovernedRecordKind::Artifact,
            b"new-artifact",
            now,
        ))
        .await
        .unwrap();
    drop(store);

    let reopened = EvidenceStore::open_governed(&path, policy()).await.unwrap();
    assert!(matches!(
        reopened
            .export_governed_records(scope.clone(), vec!["event-old".into()], now + 1)
            .await,
        Err(EvidenceError::GovernedRecordNotFound)
    ));
    let retained = reopened
        .export_governed_records(scope, vec!["artifact-new".into()], now + 1)
        .await
        .unwrap();
    assert_eq!(retained.records[0].payload, b"new-artifact");
    let audits = reopened.retention_audits(10).await.unwrap();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].action, "retention");
    assert_eq!(audits[0].record_count, 1);
}
