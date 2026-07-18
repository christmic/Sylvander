use super::*;

fn feedback_attribution() -> FeedbackAttribution {
    FeedbackAttribution {
        principal_digest: "principal-sha256".into(),
        channel_instance_id: "terminal".into(),
        transport: "unix".into(),
    }
}

fn evidence_database_contract(path: &std::path::Path) -> (i64, i64, Vec<EvidenceSchemaObject>) {
    let connection = rusqlite::Connection::open(path).unwrap();
    let application_id = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .unwrap();
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    (
        application_id,
        version,
        evidence_schema_objects(&connection).unwrap(),
    )
}

async fn assert_schema_rejected_without_repair(path: &std::path::Path) {
    let before_contract = evidence_database_contract(path);
    let before_bytes = std::fs::read(path).unwrap();
    assert!(matches!(
        EvidenceStore::open(path).await,
        Err(EvidenceError::InvalidSchema)
    ));
    assert_eq!(std::fs::read(path).unwrap(), before_bytes);
    assert_eq!(evidence_database_contract(path), before_contract);
}

async fn assert_integrity_rejected_without_repair(path: &std::path::Path) {
    let before_contract = evidence_database_contract(path);
    let before_bytes = std::fs::read(path).unwrap();
    assert!(matches!(
        EvidenceStore::open(path).await,
        Err(EvidenceError::Integrity)
    ));
    assert_eq!(std::fs::read(path).unwrap(), before_bytes);
    assert_eq!(evidence_database_contract(path), before_contract);
}

#[tokio::test]
async fn evidence_schema_is_atomic_exact_and_latest_only() {
    let directory = tempfile::tempdir().unwrap();
    let current = directory.path().join("current.db");
    drop(EvidenceStore::open(&current).await.unwrap());
    assert_eq!(
        evidence_database_contract(&current).0,
        EVIDENCE_APPLICATION_ID
    );
    assert_eq!(
        evidence_database_contract(&current).1,
        EVIDENCE_SCHEMA_VERSION
    );
    drop(EvidenceStore::open(&current).await.unwrap());

    let existing_empty = directory.path().join("existing-empty.db");
    std::fs::File::create(&existing_empty).unwrap();
    assert_schema_rejected_without_repair(&existing_empty).await;

    let unknown_database = directory.path().join("unknown-database.db");
    rusqlite::Connection::open(&unknown_database)
        .unwrap()
        .execute("CREATE TABLE unrelated_data(id INTEGER PRIMARY KEY)", [])
        .unwrap();
    assert_schema_rejected_without_repair(&unknown_database).await;

    let old = directory.path().join("old.db");
    std::fs::copy(&current, &old).unwrap();
    rusqlite::Connection::open(&old)
        .unwrap()
        .pragma_update(None, "user_version", EVIDENCE_SCHEMA_VERSION - 1)
        .unwrap();
    assert_schema_rejected_without_repair(&old).await;

    let future = directory.path().join("future.db");
    std::fs::copy(&current, &future).unwrap();
    rusqlite::Connection::open(&future)
        .unwrap()
        .pragma_update(None, "user_version", EVIDENCE_SCHEMA_VERSION + 1)
        .unwrap();
    assert_schema_rejected_without_repair(&future).await;

    let foreign = directory.path().join("foreign.db");
    std::fs::copy(&current, &foreign).unwrap();
    rusqlite::Connection::open(&foreign)
        .unwrap()
        .pragma_update(None, "application_id", EVIDENCE_APPLICATION_ID + 1)
        .unwrap();
    assert_schema_rejected_without_repair(&foreign).await;

    let unversioned = directory.path().join("unversioned.db");
    std::fs::copy(&current, &unversioned).unwrap();
    let connection = rusqlite::Connection::open(&unversioned).unwrap();
    connection.pragma_update(None, "application_id", 0).unwrap();
    connection.pragma_update(None, "user_version", 0).unwrap();
    drop(connection);
    assert_schema_rejected_without_repair(&unversioned).await;

    let partial = directory.path().join("partial.db");
    let connection = rusqlite::Connection::open(&partial).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE evidence_runs(
                id TEXT PRIMARY KEY,
                server_name TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                status TEXT NOT NULL
             )",
        )
        .unwrap();
    connection
        .pragma_update(None, "application_id", EVIDENCE_APPLICATION_ID)
        .unwrap();
    connection
        .pragma_update(None, "user_version", EVIDENCE_SCHEMA_VERSION)
        .unwrap();
    drop(connection);
    assert_schema_rejected_without_repair(&partial).await;

    let unknown = directory.path().join("unknown-object.db");
    std::fs::copy(&current, &unknown).unwrap();
    rusqlite::Connection::open(&unknown)
        .unwrap()
        .execute("CREATE TABLE unknown_evidence_object(id INTEGER)", [])
        .unwrap();
    assert_schema_rejected_without_repair(&unknown).await;

    let invalid_reference = directory.path().join("invalid-reference.db");
    std::fs::copy(&current, &invalid_reference).unwrap();
    let connection = rusqlite::Connection::open(&invalid_reference).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
        .execute(
            "INSERT INTO evidence_turns(
               id,run_id,session_id,started_at,status,input_bytes,feedback_target
             ) VALUES ('orphan-turn','missing-run','session-1',1,'running',0,'target-1')",
            [],
        )
        .unwrap();
    drop(connection);
    assert_integrity_rejected_without_repair(&invalid_reference).await;
    let connection = rusqlite::Connection::open(&invalid_reference).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM evidence_turns WHERE id='orphan-turn'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn stores_structured_run_turn_step_outcome_and_event() {
    let store = EvidenceStore::open_in_memory().await.unwrap();
    store
        .start_run("run-1".into(), "test".into(), 1)
        .await
        .unwrap();
    store
        .start_turn(TurnStart {
            id: "turn-1".into(),
            run_id: "run-1".into(),
            session_id: "session-1".into(),
            agent_id: Some("agent-1".into()),
            started_at: 2,
            input_bytes: 5,
            input_digest: Some("digest".into()),
        })
        .await
        .unwrap();
    store
        .start_step(StepStart {
            id: "tool-1".into(),
            turn_id: "turn-1".into(),
            kind: "tool".into(),
            name: "read".into(),
            started_at: 3,
            input_bytes: 2,
            input_digest: None,
        })
        .await
        .unwrap();
    store
        .finish_step("tool-1".into(), 4, "succeeded", 7)
        .await
        .unwrap();
    store
        .record_outcome(
            "outcome-1".into(),
            "turn-1".into(),
            "completed".into(),
            true,
            5,
        )
        .await
        .unwrap();
    store
        .append_event(EvidenceEvent {
            id: "event-1".into(),
            run_id: "run-1".into(),
            session_id: "session-1".into(),
            turn_id: Some("turn-1".into()),
            event_type: "done".into(),
            occurred_at: 5,
            observed_at: 5,
            payload_bytes: 7,
            payload_digest: None,
            payload_json: None,
            privacy_class: "user_content".into(),
        })
        .await
        .unwrap();
    store
        .finish_turn("turn-1".into(), 5, "succeeded", 7)
        .await
        .unwrap();
    store
        .finish_run("run-1".into(), 6, "completed")
        .await
        .unwrap();
    assert_eq!(
        store.counts().await.unwrap(),
        EvidenceCounts {
            runs: 1,
            turns: 1,
            steps: 1,
            outcomes: 1,
            events: 1
        }
    );
    let turns = store
        .query_turns(TurnQuery {
            session_id: Some("session-1".into()),
            status: Some("succeeded".into()),
            started_after: Some(1),
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].step_count, 1);
    assert_eq!(turns[0].failed_step_count, 0);
    assert_eq!(turns[0].successful_outcome, Some(true));
}

#[tokio::test]
async fn feedback_target_is_opaque_stable_and_resolves_only_through_evidence() {
    let store = EvidenceStore::open_in_memory().await.unwrap();
    store
        .start_run("run-visible-only-to-runtime".into(), "test".into(), 1)
        .await
        .unwrap();
    store
        .start_turn(TurnStart {
            id: "turn-visible-only-to-runtime".into(),
            run_id: "run-visible-only-to-runtime".into(),
            session_id: "session-1".into(),
            agent_id: Some("agent-1".into()),
            started_at: 2,
            input_bytes: 0,
            input_digest: None,
        })
        .await
        .unwrap();
    let target = feedback_target(
        "run-visible-only-to-runtime",
        "turn-visible-only-to-runtime",
    );
    assert!(target.0.starts_with("sha256:"));
    assert!(!target.0.contains("run-visible"));
    assert!(!target.0.contains("turn-visible"));
    assert_eq!(
        store.feedback_session(target).await.unwrap().as_deref(),
        Some("session-1")
    );
}

#[tokio::test]
async fn turn_usage_never_treats_missing_pricing_as_zero_cost() {
    let store = EvidenceStore::open_in_memory().await.unwrap();
    store
        .start_run("run-usage".into(), "test".into(), 1)
        .await
        .unwrap();
    store
        .start_turn(TurnStart {
            id: "turn-usage".into(),
            run_id: "run-usage".into(),
            session_id: "session-usage".into(),
            agent_id: Some("agent-1".into()),
            started_at: 2,
            input_bytes: 0,
            input_digest: None,
        })
        .await
        .unwrap();
    store
        .record_iteration_usage("turn-usage".into(), 10, 5, Some(25))
        .await
        .unwrap();
    store
        .record_iteration_usage("turn-usage".into(), 7, 3, None)
        .await
        .unwrap();

    assert_eq!(
        store.turn_usage("turn-usage".into()).await.unwrap(),
        Some(TurnUsage {
            input_tokens: 17,
            output_tokens: 8,
            cost_nano_usd: None,
            iteration_count: 2,
        })
    );
}

#[tokio::test]
async fn authorization_denials_are_durable_and_content_free() {
    let store = EvidenceStore::open_in_memory().await.unwrap();
    let denial = AuthorizationDenial {
        id: "denial-1".into(),
        occurred_at: 42,
        request_id: "request-1".into(),
        principal_digest: Some("principal-digest".into()),
        channel_instance_id: "desktop-primary".into(),
        transport: "websocket".into(),
        operation: "load_session".into(),
        code: "forbidden".into(),
        resource_digest: Some("resource-digest".into()),
    };
    store
        .record_authorization_denial(denial.clone())
        .await
        .unwrap();
    assert_eq!(store.authorization_denials(10).await.unwrap(), vec![denial]);
}

#[tokio::test]
async fn agent_administration_audit_preserves_pending_and_terminal_outcomes() {
    let store = EvidenceStore::open_in_memory().await.unwrap();
    let pending = AgentAdministrationAudit {
        id: "admin-1".into(),
        occurred_at: 43,
        request_id: "request-2".into(),
        principal_digest: "principal-digest".into(),
        channel_instance_id: "admin-console".into(),
        operation: "activate_revision".into(),
        agent_digest: "agent-digest".into(),
        revision: 2,
        expected_active_revision: 1,
        outcome: "pending".into(),
        error_code: None,
    };
    store
        .begin_agent_administration(pending.clone())
        .await
        .unwrap();
    assert_eq!(
        store.agent_administration_audits(10).await.unwrap(),
        vec![pending]
    );
    store
        .finish_agent_administration("admin-1".into(), "succeeded", None)
        .await
        .unwrap();
    let completed = store.agent_administration_audits(10).await.unwrap();
    assert_eq!(completed[0].outcome, "succeeded");
    assert!(completed[0].error_code.is_none());
    assert!(matches!(
        store
            .finish_agent_administration("admin-1".into(), "failed", None)
            .await,
        Err(EvidenceError::InvalidAuditState)
    ));
}

#[tokio::test]
async fn generic_administration_audit_is_restart_durable_and_content_free() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("evidence.db");
    let audit = AdministrationAudit {
        id: "registry-admin-1".into(),
        occurred_at: 44,
        request_id: "request-3".into(),
        principal_digest: "principal-sha256".into(),
        channel_instance_id: "admin-console".into(),
        transport: "unix".into(),
        operation: "activate".into(),
        resource_kind: "provider".into(),
        resource_digest: "resource-sha256".into(),
        version: Some(7),
        outcome: "failed".into(),
        error_code: Some("revision_conflict".into()),
    };
    let store = EvidenceStore::open(&path).await.unwrap();
    store
        .record_administration_audit(audit.clone())
        .await
        .unwrap();
    let list_audit = AdministrationAudit {
        id: "registry-admin-list".into(),
        occurred_at: 45,
        request_id: "request-4".into(),
        principal_digest: "principal-sha256".into(),
        channel_instance_id: "admin-console".into(),
        transport: "unix".into(),
        operation: "list".into(),
        resource_kind: "provider".into(),
        resource_digest: "provider-collection-sha256".into(),
        version: None,
        outcome: "succeeded".into(),
        error_code: None,
    };
    store
        .record_administration_audit(list_audit.clone())
        .await
        .unwrap();
    drop(store);

    let reopened = EvidenceStore::open(&path).await.unwrap();
    assert_eq!(
        reopened.administration_audits(10).await.unwrap(),
        vec![list_audit, audit]
    );
    drop(reopened);

    let database = std::fs::read(path).unwrap();
    for marker in [
        b"https://provider.internal.example".as_slice(),
        b"provider:alpha:api_key".as_slice(),
        b"raw-provider-id".as_slice(),
    ] {
        assert!(
            !database
                .windows(marker.len())
                .any(|window| window == marker)
        );
    }
}

#[tokio::test]
async fn administration_mutation_intent_survives_crash_and_finishes_once() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("mutation-audit.db");
    let pending = AdministrationAudit {
        id: "registry-mutation-1".into(),
        occurred_at: 50,
        request_id: "request-5".into(),
        principal_digest: "principal-sha256".into(),
        channel_instance_id: "admin-console".into(),
        transport: "unix".into(),
        operation: "activate_credential_generation".into(),
        resource_kind: "credential".into(),
        resource_digest: "binding-sha256".into(),
        version: Some(3),
        outcome: "pending".into(),
        error_code: None,
    };
    let store = EvidenceStore::open(&path).await.unwrap();
    store
        .begin_administration_mutation(pending.clone())
        .await
        .unwrap();
    drop(store);

    let reopened = EvidenceStore::open(&path).await.unwrap();
    assert_eq!(reopened.administration_audits(10).await.unwrap(), [pending]);
    reopened
        .finish_administration_mutation(
            "registry-mutation-1".into(),
            "failed",
            Some("active_generation_conflict".into()),
        )
        .await
        .unwrap();
    let terminal = reopened.administration_audits(10).await.unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].outcome, "failed");
    assert_eq!(
        terminal[0].error_code.as_deref(),
        Some("active_generation_conflict")
    );
    assert!(matches!(
        reopened
            .finish_administration_mutation("registry-mutation-1".into(), "succeeded", None,)
            .await,
        Err(EvidenceError::InvalidAuditState)
    ));
}

#[tokio::test]
async fn feedback_requires_traceable_run_and_turn_evidence() {
    let store = EvidenceStore::open_in_memory().await.unwrap();
    store
        .start_run("run-1".into(), "test".into(), 1)
        .await
        .unwrap();
    store
        .start_turn(TurnStart {
            id: "turn-1".into(),
            run_id: "run-1".into(),
            session_id: "session-1".into(),
            agent_id: Some("agent-1".into()),
            started_at: 2,
            input_bytes: 0,
            input_digest: None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .feedback_session(feedback_target("run-1", "turn-1"))
            .await
            .unwrap(),
        Some("session-1".into())
    );

    let feedback_id = store
        .record_feedback(
            RunFeedback {
                target: feedback_target("run-1", "turn-1"),
                rating: FeedbackRating::Positive,
                note: Some("useful".into()),
                correction: Some("keep the smaller patch".into()),
                tags: vec!["correct".into()],
                task_result: Some(FeedbackTaskResult::Succeeded),
                artifacts: vec![EvidenceReference {
                    locator: "worktree:session-1".into(),
                    digest_sha256: None,
                }],
                validations: vec![EvidenceReference {
                    locator: "test:cargo-test".into(),
                    digest_sha256: Some("a".repeat(64)),
                }],
                privacy_class: sylvander_protocol::FeedbackPrivacyClass::Private,
            },
            feedback_attribution(),
            3,
        )
        .await
        .unwrap();
    assert!(!feedback_id.is_empty());
    assert_eq!(store.feedback_count().await.unwrap(), 1);
    let stored = store.feedback(feedback_id).await.unwrap().unwrap();
    assert_eq!(stored.correction.as_deref(), Some("keep the smaller patch"));
    assert_eq!(stored.task_result, Some(FeedbackTaskResult::Succeeded));
    assert_eq!(stored.artifacts[0].locator, "worktree:session-1");
    assert_eq!(stored.attribution, feedback_attribution());

    let error = store
        .record_feedback(
            RunFeedback {
                target: feedback_target("run-1", "unknown-turn"),
                rating: FeedbackRating::Negative,
                note: None,
                correction: None,
                tags: Vec::new(),
                task_result: None,
                artifacts: Vec::new(),
                validations: Vec::new(),
                privacy_class: sylvander_protocol::FeedbackPrivacyClass::Private,
            },
            feedback_attribution(),
            4,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, EvidenceError::InvalidFeedbackTarget));
    assert_eq!(store.feedback_count().await.unwrap(), 1);
}

#[tokio::test]
async fn reopening_marks_inflight_records_interrupted() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("evidence.db");
    let store = EvidenceStore::open(&path).await.unwrap();
    store
        .start_run("run-1".into(), "test".into(), 1)
        .await
        .unwrap();
    store
        .start_turn(TurnStart {
            id: "turn-1".into(),
            run_id: "run-1".into(),
            session_id: "session-1".into(),
            agent_id: None,
            started_at: 2,
            input_bytes: 0,
            input_digest: None,
        })
        .await
        .unwrap();
    drop(store);

    let reopened = EvidenceStore::open(path).await.unwrap();
    assert_eq!(
        reopened.turn_status("turn-1".into()).await.unwrap(),
        Some("interrupted".into())
    );
}

#[tokio::test]
async fn retention_removes_only_completed_old_runs() {
    let store = EvidenceStore::open_in_memory().await.unwrap();
    store
        .start_run("old".into(), "test".into(), 1)
        .await
        .unwrap();
    store
        .finish_run("old".into(), 2, "completed")
        .await
        .unwrap();
    store
        .start_run("active".into(), "test".into(), 1)
        .await
        .unwrap();

    assert_eq!(store.prune_before(3).await.unwrap(), 1);
    assert_eq!(store.counts().await.unwrap().runs, 1);
}
