use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::Connection;
use serde_json::json;
use sylvander_agent::session_store::SessionLifetime;
use sylvander_agent::tool_context::ToolContext;
use sylvander_protocol::{
    AgentId, SessionContext, SessionId, SessionMetadata, UserId, UserProfileData,
};

use super::*;
use crate::capability_runtime::{
    CapabilityAuditOutcome, CapabilityAuditRecord, CapabilityAuditSink,
};

fn now() -> i64 {
    sylvander_agent::session::now_secs()
}

fn digest(seed: char) -> String {
    format!("sha256:{}", seed.to_string().repeat(64))
}

fn settings(directory: &std::path::Path, revision: u64) -> GuardianRuntimeSettings {
    GuardianRuntimeSettings {
        curation_path: directory.join("curation.db"),
        canonical_path: directory.join("canonical.db"),
        service_id: "guardian.runtime".into(),
        initial_credential_revision: revision,
        policy_revision: 1,
        curator_version: "builtin-reference-v1".into(),
        identity_ttl_secs: 3_600,
        lease_secs: 30,
        retry_delay_secs: 1,
        max_attempts: 3,
        poll_interval: Duration::from_hours(1),
    }
}

async fn preferences(directory: &std::path::Path) -> Arc<dyn LearningPreferenceSource> {
    Arc::new(
        UserProfileStore::open(directory.join("profiles.db"))
            .await
            .unwrap(),
    )
}

fn event(event_id: &str, occurred_at: i64) -> GuardianEvent {
    GuardianEvent::new(
        event_id,
        crate::guardian_curation::GuardianEventKind::SessionClosed,
        RuntimeOwnerScope::guardian(
            AgentId::new("agent-a"),
            None,
            BTreeSet::from(["workspace-a".into()]),
        ),
        vec![EvidenceReference {
            kind: "session".into(),
            reference: "session-a".into(),
            digest: digest('a'),
        }],
        digest('b'),
        occurred_at,
    )
}

fn user_event(event_id: &str, user_id: &str, occurred_at: i64) -> GuardianEvent {
    GuardianEvent::new(
        event_id,
        GuardianEventKind::SessionClosed,
        RuntimeOwnerScope::guardian(
            AgentId::new("agent-a"),
            Some(UserId::new(user_id)),
            BTreeSet::from(["workspace-a".into()]),
        ),
        vec![EvidenceReference {
            kind: "session".into(),
            reference: "session-a".into(),
            digest: digest('a'),
        }],
        digest('b'),
        occurred_at,
    )
}

fn stored_session(session_id: &str) -> StoredSession {
    StoredSession::new(
        SessionId::new(session_id),
        "guardian",
        SessionLifetime::Persistent,
        SessionMetadata {
            workspace: PathBuf::from("/workspace"),
            name: "guardian".into(),
            user_id: "user-a".into(),
        },
        vec![AgentId::new("agent-a")],
    )
}

async fn enqueue_candidate(
    service: &GuardianRuntime,
    event_id: &str,
    scope: CandidateScope,
    text: &str,
    workspace_id: Option<&str>,
    occurred_at: i64,
) {
    let event = staged_candidate_event(
        service,
        event_id,
        "user-a",
        scope,
        text,
        workspace_id,
        occurred_at,
    )
    .await;
    assert!(service.enqueue_event(event, occurred_at).await.unwrap());
}

async fn staged_candidate_event(
    service: &GuardianRuntime,
    event_id: &str,
    user_id: &str,
    scope: CandidateScope,
    text: &str,
    workspace_id: Option<&str>,
    occurred_at: i64,
) -> GuardianEvent {
    let content = json!({"text": text, "tags": ["test"]});
    let payload_digest =
        digest_value(b"sylvander.guardian.learning-payload.v1\0", &content).unwrap();
    let intake_id = format!("intake:{event_id}");
    let workspace_ids = workspace_id
        .map(|id| BTreeSet::from([id.to_owned()]))
        .unwrap_or_default();
    let owner = RuntimeOwnerScope::guardian(
        AgentId::new("agent-a"),
        Some(UserId::new(user_id)),
        workspace_ids,
    );
    let evidence = vec![EvidenceReference {
        kind: "worker_memory_candidate".into(),
        reference: intake_id.clone(),
        digest: payload_digest.clone(),
    }];
    let payload = StagedCandidatePayload {
        origin_session_id: "profile-confirmation".into(),
        source_key: intake_id.clone(),
        content,
        evidence: evidence.clone(),
        origin: CandidateOrigin::Explicit,
        classification: StagedCandidateClassification {
            scope,
            confidence_basis_points: 10_000,
            sensitivity: if scope == CandidateScope::UserProfile {
                Sensitivity::Personal
            } else {
                Sensitivity::Internal
            },
            retention_secs: BUILTIN_RETENTION_SECS,
            dedupe_key: format!("dedupe:{event_id}"),
            workspace_id: workspace_id.map(str::to_owned),
        },
    };
    service
        .inner
        .canonical
        .stage_payload(&intake_id, &owner, &payload, &payload_digest, occurred_at)
        .await
        .unwrap();
    GuardianEvent::new(
        event_id,
        GuardianEventKind::MemoryCandidateCreated,
        owner,
        evidence,
        payload_digest,
        occurred_at,
    )
}

#[tokio::test]
async fn actor_snapshots_are_disjoint_immutable_and_runtime_owned() {
    let directory = tempfile::tempdir().unwrap();
    let service = GuardianRuntime::start(
        settings(directory.path(), 1),
        now(),
        preferences(directory.path()).await,
    )
    .await
    .unwrap();
    let worker = service
        .worker_snapshot(
            &SessionContext::new("user-a", "agent-a", "session-a"),
            BTreeSet::from(["workspace-a".into()]),
        )
        .await;
    let guardian = {
        let epoch = service.inner.epoch.read().await;
        epoch
            .capabilities
            .begin_guardian_run(
                &epoch.identity,
                RuntimeOwnerScope::guardian(
                    AgentId::new("agent-a"),
                    None,
                    BTreeSet::from(["workspace-a".into()]),
                ),
                now(),
            )
            .unwrap()
    };

    assert_eq!(worker.actor(), CapabilityActor::Worker);
    assert_eq!(guardian.actor(), CapabilityActor::Guardian);
    assert_ne!(worker.revision(), guardian.revision());
    assert_eq!(worker.definitions()[0].name, "worker.runtime_metadata");
    assert_eq!(guardian.definitions()[0].name, "guardian.runtime_metadata");
    assert_eq!(
        worker
            .invoke("worker.runtime_metadata", &json!({}), now())
            .await
            .unwrap(),
        json!({
            "actor": "worker",
            "user_bound": true,
            "session_bound": true,
            "workspace_count": 1
        })
    );
    assert_eq!(
        worker
            .invoke("guardian.runtime_metadata", &json!({}), now())
            .await,
        Err(CapabilityRuntimeError::CapabilityUnavailable)
    );

    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn session_events_complete_without_fabricating_canonical_memory() {
    let directory = tempfile::tempdir().unwrap();
    let timestamp = now();
    let service = GuardianRuntime::start(
        settings(directory.path(), 1),
        timestamp,
        preferences(directory.path()).await,
    )
    .await
    .unwrap();
    service
        .enqueue_event(event("event-a", timestamp), timestamp)
        .await
        .unwrap();

    for offset in 0..20 {
        let _ = service.drain_once(timestamp + offset).await.unwrap();
        if outbox_state(directory.path(), "event-a").as_deref() == Some("completed") {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert_eq!(canonical_count(&service), 0);
    assert_eq!(
        outbox_state(directory.path(), "event-a").as_deref(),
        Some("completed")
    );
    assert!(service.last_error().await.is_none());
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn persisted_feedback_reference_is_completed_without_placeholder_memory() {
    let directory = tempfile::tempdir().unwrap();
    let timestamp = now();
    let service = GuardianRuntime::start(
        settings(directory.path(), 1),
        timestamp,
        preferences(directory.path()).await,
    )
    .await
    .unwrap();
    let session = StoredSession::new(
        SessionId::new("session-feedback"),
        "feedback",
        SessionLifetime::Persistent,
        SessionMetadata {
            workspace: PathBuf::from("/workspace"),
            name: "feedback".into(),
            user_id: "user-a".into(),
        },
        vec![AgentId::new("agent-a")],
    );

    assert!(
        service
            .enqueue_feedback(&session, "feedback-a", &digest('f'), timestamp)
            .await
            .unwrap()
    );
    for offset in 0..20 {
        let _ = service.drain_once(timestamp + offset).await.unwrap();
        if outbox_state(directory.path(), "feedback:feedback-a").as_deref() == Some("completed") {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert_eq!(canonical_count(&service), 0);
    assert_eq!(
        outbox_state(directory.path(), "feedback:feedback-a").as_deref(),
        Some("completed")
    );
    assert!(service.last_error().await.is_none());
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn production_session_close_also_schedules_owner_retention() {
    let directory = tempfile::tempdir().unwrap();
    let timestamp = now();
    let service = GuardianRuntime::start(
        settings(directory.path(), 1),
        timestamp,
        preferences(directory.path()).await,
    )
    .await
    .unwrap();
    assert!(
        service
            .enqueue_session_closed(&stored_session("retention-cadence"), timestamp)
            .await
            .unwrap()
    );
    let connection = Connection::open(directory.path().join("curation.db")).unwrap();
    let event_kinds = {
        let mut statement = connection
            .prepare("SELECT event_kind FROM guardian_outbox ORDER BY event_kind")
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(event_kinds, ["retention_sweep", "session_closed"]);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn typed_candidates_commit_all_four_scopes_and_become_retrievable_context() {
    let directory = tempfile::tempdir().unwrap();
    let timestamp = now();
    let service = GuardianRuntime::start(
        settings(directory.path(), 1),
        timestamp,
        preferences(directory.path()).await,
    )
    .await
    .unwrap();
    let cases = [
        (
            "candidate-relationship",
            CandidateScope::Relationship,
            "guardian relationship",
            None,
        ),
        (
            "candidate-canonical",
            CandidateScope::AgentCanonical,
            "guardian canonical",
            None,
        ),
        (
            "candidate-workspace",
            CandidateScope::WorkspaceKnowledge,
            "guardian workspace",
            Some("workspace-a"),
        ),
    ];
    for (event_id, scope, text, workspace_id) in cases {
        enqueue_candidate(&service, event_id, scope, text, workspace_id, timestamp).await;
        assert!(service.drain_once(timestamp).await.unwrap());
    }

    enqueue_candidate(
        &service,
        "candidate-profile",
        CandidateScope::UserProfile,
        "guardian profile",
        None,
        timestamp,
    )
    .await;
    for offset in 0..20 {
        let _ = service.drain_once(timestamp + offset).await.unwrap();
        let state = Connection::open(directory.path().join("curation.db"))
            .unwrap()
            .query_row(
                "SELECT state FROM memory_candidates
                 WHERE source_key='intake:candidate-profile'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .unwrap();
        if state.as_deref() == Some("awaiting_confirmation") {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(canonical_count(&service), 3);
    let (candidate_id, revision, state): (String, i64, String) =
        Connection::open(directory.path().join("curation.db"))
            .unwrap()
            .query_row(
                "SELECT candidate_id,revision,state FROM memory_candidates
                 WHERE source_key='intake:candidate-profile'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
    assert_eq!(state, "awaiting_confirmation");
    let session = stored_session("profile-confirmation");
    assert!(
        service
            .enqueue_confirmation(
                &session,
                &candidate_id,
                u64::try_from(revision).unwrap(),
                true,
                timestamp + 1,
            )
            .await
            .unwrap()
    );
    assert!(
        !service
            .enqueue_confirmation(
                &session,
                &candidate_id,
                u64::try_from(revision).unwrap(),
                true,
                timestamp + 1,
            )
            .await
            .unwrap()
    );
    assert!(service.drain_once(timestamp + 1).await.unwrap());
    assert!(service.drain_once(timestamp + 1).await.unwrap());
    assert_eq!(canonical_count(&service), 4);

    let entries = service
        .inner
        .canonical
        .retrieve(
            &CuratedContextSubject {
                user_id: UserId::new("user-a"),
                agent_id: AgentId::new("agent-a"),
                session_id: SessionId::new("context-session"),
                workspace_ids: vec!["workspace-a".into()],
            },
            "guardian",
            16,
        )
        .await
        .unwrap();
    assert_eq!(entries.len(), 4);
    for scope in [
        CuratedMemoryScope::Relationship,
        CuratedMemoryScope::UserProfile,
        CuratedMemoryScope::AgentCanonical,
        CuratedMemoryScope::WorkspaceKnowledge,
    ] {
        assert!(entries.iter().any(|entry| entry.scope == scope));
    }
    assert!(entries.iter().all(|entry| entry.relevance > 0));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn opt_out_after_confirmation_request_resolves_waiting_candidate_as_denied() {
    let directory = tempfile::tempdir().unwrap();
    let timestamp = now();
    let profiles = UserProfileStore::open(directory.path().join("profiles.db"))
        .await
        .unwrap();
    let created = profiles
        .create(UserId::new("user-a"), UserProfileData::default())
        .await
        .unwrap();
    let service = GuardianRuntime::start(
        settings(directory.path(), 1),
        timestamp,
        Arc::new(profiles.clone()),
    )
    .await
    .unwrap();
    enqueue_candidate(
        &service,
        "candidate-profile-opt-out",
        CandidateScope::UserProfile,
        "guardian private profile",
        None,
        timestamp,
    )
    .await;
    for offset in 0..20 {
        let _ = service.drain_once(timestamp + offset).await.unwrap();
        let state = Connection::open(directory.path().join("curation.db"))
            .unwrap()
            .query_row(
                "SELECT state FROM memory_candidates
                 WHERE source_key='intake:candidate-profile-opt-out'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .unwrap();
        if state.as_deref() == Some("awaiting_confirmation") {
            break;
        }
        tokio::task::yield_now().await;
    }
    let (candidate_id, revision): (String, i64) =
        Connection::open(directory.path().join("curation.db"))
            .unwrap()
            .query_row(
                "SELECT candidate_id,revision FROM memory_candidates
                 WHERE source_key='intake:candidate-profile-opt-out'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    profiles
        .set_do_not_learn(UserId::new("user-a"), created.revision, true)
        .await
        .unwrap();

    assert!(
        service
            .enqueue_confirmation(
                &stored_session("profile-confirmation"),
                &candidate_id,
                u64::try_from(revision).unwrap(),
                true,
                timestamp + 1,
            )
            .await
            .unwrap()
    );
    assert!(service.drain_once(timestamp + 1).await.unwrap());
    assert!(service.drain_once(timestamp + 1).await.unwrap());
    let state: String = Connection::open(directory.path().join("curation.db"))
        .unwrap()
        .query_row(
            "SELECT state FROM memory_candidates WHERE candidate_id=?1",
            [&candidate_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "rejected");
    assert_eq!(canonical_count(&service), 0);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn pending_confirmation_is_session_bound_and_decisions_are_single_use() {
    let directory = tempfile::tempdir().unwrap();
    let timestamp = now();
    let service = GuardianRuntime::start(
        settings(directory.path(), 1),
        timestamp,
        preferences(directory.path()).await,
    )
    .await
    .unwrap();
    enqueue_candidate(
        &service,
        "candidate-confirmation-ui",
        CandidateScope::UserProfile,
        "prefers concise answers",
        None,
        timestamp,
    )
    .await;
    let owner_session = stored_session("profile-confirmation");
    let pending = service
        .pending_confirmations(&owner_session, timestamp)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].summary, "prefers concise answers");

    let other_session = stored_session("other-session");
    assert!(
        service
            .pending_confirmations(&other_session, timestamp)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        service
            .resolve_confirmation(
                &other_session,
                &pending[0].candidate_id,
                pending[0].expected_revision,
                sylvander_protocol::MemoryConfirmationDecision::Confirm,
                timestamp + 1,
            )
            .await,
        Err(GuardianRuntimeError::Curation(
            GuardianCurationError::AccessDenied
        ))
    ));

    let mut other_owner = stored_session("profile-confirmation");
    other_owner.metadata.user_id = "user-b".into();
    assert!(matches!(
        service
            .resolve_confirmation(
                &other_owner,
                &pending[0].candidate_id,
                pending[0].expected_revision,
                sylvander_protocol::MemoryConfirmationDecision::Confirm,
                timestamp + 1,
            )
            .await,
        Err(GuardianRuntimeError::Curation(
            GuardianCurationError::AccessDenied
        ))
    ));

    service
        .resolve_confirmation(
            &owner_session,
            &pending[0].candidate_id,
            pending[0].expected_revision,
            sylvander_protocol::MemoryConfirmationDecision::Reject,
            timestamp + 1,
        )
        .await
        .unwrap();
    assert!(matches!(
        service
            .resolve_confirmation(
                &owner_session,
                &pending[0].candidate_id,
                pending[0].expected_revision,
                sylvander_protocol::MemoryConfirmationDecision::Reject,
                timestamp + 2,
            )
            .await,
        Err(GuardianRuntimeError::Curation(
            GuardianCurationError::Conflict
        ))
    ));
    assert!(
        service
            .pending_confirmations(&owner_session, timestamp + 2)
            .await
            .unwrap()
            .is_empty()
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn retention_event_expires_only_governed_owner_records() {
    let directory = tempfile::tempdir().unwrap();
    let timestamp = now();
    let service = GuardianRuntime::start(
        settings(directory.path(), 1),
        timestamp,
        preferences(directory.path()).await,
    )
    .await
    .unwrap();
    enqueue_candidate(
        &service,
        "candidate-retention",
        CandidateScope::WorkspaceKnowledge,
        "guardian retention",
        Some("workspace-a"),
        timestamp,
    )
    .await;
    assert!(service.drain_once(timestamp).await.unwrap());
    {
        let connection = service.inner.canonical.connection.lock().unwrap();
        connection
            .execute(
                "UPDATE guardian_canonical_memory SET expires_at=?1",
                [timestamp],
            )
            .unwrap();
    }
    assert!(
        service
            .enqueue_retention_sweep(
                &stored_session("retention"),
                BTreeSet::from(["workspace-a".into()]),
                timestamp + 1,
            )
            .await
            .unwrap()
    );
    assert!(service.drain_once(timestamp + 1).await.unwrap());
    assert_eq!(canonical_count(&service), 0);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn expired_run_lease_is_reclaimed_after_restart_with_rotated_credential() {
    let directory = tempfile::tempdir().unwrap();
    let timestamp = now();
    let first_settings = settings(directory.path(), 1);
    let epoch = GuardianEpoch::open(&first_settings, 1, timestamp)
        .await
        .unwrap();
    epoch
        .store
        .enqueue_event(event("event-restart", timestamp), timestamp)
        .await
        .unwrap();
    let abandoned = epoch
        .store
        .claim_next_run(&epoch.identity, "builtin-reference-v1", timestamp, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(abandoned.attempt, 1);
    drop(epoch);

    let restarted = GuardianRuntime::start(
        settings(directory.path(), 2),
        timestamp + 2,
        preferences(directory.path()).await,
    )
    .await
    .unwrap();
    assert!(restarted.drain_once(timestamp + 2).await.unwrap());

    assert_eq!(canonical_count(&restarted), 0);
    assert_eq!(
        outbox_state(directory.path(), "event-restart").as_deref(),
        Some("completed")
    );
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_sink_replays_the_same_idempotency_key_exactly_once() {
    let directory = tempfile::tempdir().unwrap();
    let store = GuardianCanonicalStore::open(directory.path().join("canonical.db"))
        .await
        .unwrap();
    let mutation = ClaimedMutation {
        mutation_id: "mutation-a".into(),
        candidate_id: "candidate-a".into(),
        candidate_revision: 4,
        action: MutationAction::Commit,
        scope: CandidateScope::AgentCanonical,
        owner_user_id: None,
        owner_agent_id: AgentId::new("agent-a"),
        workspace_id: None,
        body: json!({
            "action": "commit",
            "candidate_id": "candidate-a",
            "candidate_revision": 4,
            "content": {"event_reference": "event-a"},
            "content_digest": digest('c'),
            "expires_at_unix_secs": now() + 60,
            "retention_secs": 60
        }),
        idempotency_key: digest('d'),
        claim_token: "claim-a".into(),
        attempt: 1,
        lease_expires_at_unix_secs: now() + 30,
    };

    store.apply(&mutation, now()).await.unwrap();
    store.apply(&mutation, now() + 1).await.unwrap();

    let count = {
        let connection = store.connection.lock().unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM guardian_mutation_receipts",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    };
    assert_eq!(count, 1);
}

#[tokio::test]
async fn learning_opt_out_blocks_new_and_preexisting_events_before_candidate_extraction() {
    let directory = tempfile::tempdir().unwrap();
    let timestamp = now();
    let profiles = UserProfileStore::open(directory.path().join("profiles.db"))
        .await
        .unwrap();
    let owner = UserId::new("alice");
    let created = profiles
        .create(owner.clone(), UserProfileData::default())
        .await
        .unwrap();
    profiles
        .set_do_not_learn(owner, created.revision, true)
        .await
        .unwrap();
    let service = GuardianRuntime::start(
        settings(directory.path(), 51),
        timestamp,
        Arc::new(profiles),
    )
    .await
    .unwrap();

    assert!(
        !service
            .enqueue_event(
                user_event("blocked-before-enqueue", "alice", timestamp),
                timestamp,
            )
            .await
            .unwrap()
    );
    let blocked_count: i64 = Connection::open(directory.path().join("curation.db"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM guardian_outbox WHERE event_id='blocked-before-enqueue'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(blocked_count, 0);

    let persisted = staged_candidate_event(
        &service,
        "persisted-before-opt-out",
        "alice",
        CandidateScope::Relationship,
        "must remain blocked",
        None,
        timestamp,
    )
    .await;
    {
        let epoch = service.inner.epoch.read().await;
        assert!(
            epoch
                .store
                .enqueue_event(persisted, timestamp)
                .await
                .unwrap()
        );
    }
    for offset in 0..10 {
        let _ = service.drain_once(timestamp + offset).await.unwrap();
        let state: Option<String> = Connection::open(directory.path().join("curation.db"))
            .unwrap()
            .query_row(
                "SELECT state FROM guardian_outbox WHERE event_id='persisted-before-opt-out'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        if state.as_deref() == Some("completed") {
            break;
        }
        tokio::task::yield_now().await;
    }

    let connection = Connection::open(directory.path().join("curation.db")).unwrap();
    let state: String = connection
        .query_row(
            "SELECT state FROM guardian_outbox WHERE event_id='persisted-before-opt-out'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let candidate_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM memory_candidates", [], |row| {
            row.get(0)
        })
        .unwrap();
    let opt_out_audits: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM guardian_curation_audit
             WHERE event_id='persisted-before-opt-out'
               AND operation='run_completed' AND reason_code='learning_opt_out'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(candidate_count, 0);
    assert_eq!(state, "completed");
    assert_eq!(opt_out_audits, 1);
    assert_eq!(canonical_count(&service), 0);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn unavailable_profile_source_fails_closed_before_guardian_event_persistence() {
    let directory = tempfile::tempdir().unwrap();
    let timestamp = now();
    let service = GuardianRuntime::start(
        settings(directory.path(), 53),
        timestamp,
        Arc::new(FixedLearningPreference(Err(()))),
    )
    .await
    .unwrap();
    assert_eq!(
        service
            .enqueue_event(
                user_event("profile-unavailable", "private-user", timestamp),
                timestamp,
            )
            .await,
        Err(GuardianRuntimeError::LearningPreferenceUnavailable)
    );
    let event_count: i64 = Connection::open(directory.path().join("curation.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM guardian_outbox", [], |row| row.get(0))
        .unwrap();
    assert_eq!(event_count, 0);
    assert!(
        !GuardianRuntimeError::LearningPreferenceUnavailable
            .to_string()
            .contains("private-user")
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn learned_commit_gate_covers_every_scope_and_preserves_governance_actions() {
    let blocked = FixedLearningPreference(Ok(true));
    for scope in [
        CandidateScope::Relationship,
        CandidateScope::UserProfile,
        CandidateScope::AgentCanonical,
        CandidateScope::WorkspaceKnowledge,
    ] {
        assert!(
            !learned_write_allowed(
                &blocked,
                Some(&UserId::new("alice")),
                scope,
                MutationAction::Commit,
            )
            .await
            .unwrap()
        );
        for action in [
            MutationAction::Correct,
            MutationAction::Decay,
            MutationAction::Forget,
        ] {
            assert!(
                learned_write_allowed(&blocked, Some(&UserId::new("alice")), scope, action,)
                    .await
                    .unwrap()
            );
        }
    }
    assert!(
        learned_write_allowed(
            &blocked,
            None,
            CandidateScope::AgentCanonical,
            MutationAction::Commit,
        )
        .await
        .unwrap()
    );
    assert_eq!(
        learned_write_allowed(
            &FixedLearningPreference(Err(())),
            Some(&UserId::new("alice")),
            CandidateScope::UserProfile,
            MutationAction::Commit,
        )
        .await,
        Err(GuardianRuntimeError::LearningPreferenceUnavailable)
    );
}

#[tokio::test]
async fn opt_out_after_scheduling_prevents_the_real_canonical_sink_write() {
    let directory = tempfile::tempdir().unwrap();
    let timestamp = now();
    let runtime_settings = settings(directory.path(), 52);
    let profiles = UserProfileStore::open(directory.path().join("profiles.db"))
        .await
        .unwrap();
    let owner = UserId::new("alice");
    let created = profiles
        .create(owner.clone(), UserProfileData::default())
        .await
        .unwrap();
    let epoch = GuardianEpoch::open(&runtime_settings, 52, timestamp)
        .await
        .unwrap();
    let canonical = GuardianCanonicalStore::open(&runtime_settings.canonical_path)
        .await
        .unwrap();
    let runtime = GuardianRuntimeInner {
        settings: runtime_settings,
        epoch: RwLock::new(epoch),
        canonical,
        learning_preferences: Arc::new(profiles.clone()),
        last_error: RwLock::new(None),
    };
    let epoch = runtime.epoch.read().await;
    epoch
        .store
        .enqueue_event(user_event("scheduled", "alice", timestamp), timestamp)
        .await
        .unwrap();
    let claim = epoch
        .store
        .claim_next_run(&epoch.identity, "builtin-reference-v1", timestamp, 30)
        .await
        .unwrap()
        .unwrap();
    let candidate = epoch
        .store
        .extract_candidate(
            &epoch.identity,
            &claim,
            CandidateDraft {
                source_key: "scheduled".into(),
                content: json!({"fact": "scheduled"}),
                evidence: vec![EvidenceReference {
                    kind: "session".into(),
                    reference: "session-a".into(),
                    digest: digest('c'),
                }],
                origin: CandidateOrigin::Explicit,
            },
            timestamp,
        )
        .await
        .unwrap();
    let candidate = epoch
        .store
        .classify_candidate(
            &epoch.identity,
            &claim,
            &candidate.candidate_id,
            candidate.revision,
            CandidateClassification {
                scope: CandidateScope::AgentCanonical,
                confidence_basis_points: 10_000,
                sensitivity: Sensitivity::Internal,
                retention_secs: 60,
                dedupe_key: "scheduled".into(),
                workspace_id: None,
            },
            timestamp,
        )
        .await
        .unwrap();
    let candidate = epoch
        .store
        .reconcile_candidate(
            &epoch.identity,
            &claim,
            &candidate.candidate_id,
            candidate.revision,
            Reconciliation::Unique,
            timestamp,
        )
        .await
        .unwrap();
    let decision = epoch
        .store
        .evaluate_policy(
            &epoch.identity,
            &claim,
            &candidate.candidate_id,
            candidate.revision,
            timestamp,
        )
        .await
        .unwrap();
    assert_eq!(decision.outcome, PolicyOutcome::Allow);
    let candidate = epoch
        .store
        .candidate(&candidate.candidate_id)
        .await
        .unwrap();
    epoch
        .store
        .schedule_mutation(
            &epoch.identity,
            &claim,
            &candidate.candidate_id,
            candidate.revision,
            timestamp,
        )
        .await
        .unwrap();
    let blocked = profiles
        .set_do_not_learn(owner, created.revision, true)
        .await
        .unwrap();
    assert!(blocked.do_not_learn);

    runtime.deliver_mutations(&epoch, timestamp).await.unwrap();
    let candidate = epoch
        .store
        .candidate(&candidate.candidate_id)
        .await
        .unwrap();
    assert_eq!(candidate.state, CandidateState::DeliveryFailed);
    assert_eq!(
        runtime
            .canonical
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM guardian_canonical_memory",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn explicit_profile_correction_export_and_delete_ignore_learning_opt_out() {
    let directory = tempfile::tempdir().unwrap();
    let store = UserProfileStore::open(directory.path().join("profiles.db"))
        .await
        .unwrap();
    let owner = UserId::new("alice");
    let created = store
        .create(owner.clone(), UserProfileData::default())
        .await
        .unwrap();
    let blocked = store
        .set_do_not_learn(owner.clone(), created.revision, true)
        .await
        .unwrap();
    let corrected = store
        .correct(owner.clone(), blocked.revision, UserProfileData::default())
        .await
        .unwrap();
    assert!(corrected.do_not_learn);
    assert!(
        store
            .export(owner.clone())
            .await
            .unwrap()
            .profile
            .do_not_learn
    );
    let deleted_revision = store
        .delete(owner.clone(), corrected.revision)
        .await
        .unwrap();
    assert!(deleted_revision > corrected.revision);
    assert_eq!(store.read(owner).await.unwrap(), None);
}

fn tool_descriptor(name: &str, class: ToolInvocationClass) -> ToolInvocationDescriptor {
    ToolInvocationDescriptor {
        name: name.into(),
        class,
        input_schema: json!({"type": "object"}),
    }
}

fn tool_request(
    gateway: &Arc<dyn ToolInvocationGateway>,
    call_id: &str,
    route: &str,
    class: Option<ToolInvocationClass>,
    context: &ToolContext,
    input: serde_json::Value,
) -> ToolInvocationRequest {
    ToolInvocationRequest::new(
        call_id,
        route,
        class,
        context,
        input,
        gateway
            .snapshot()
            .for_turn("sha256:test-tool-surface", ["review-guidelines".into()]),
    )
}

fn durable_audit_rows(path: &std::path::Path) -> Vec<(String, String, String, String, String)> {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT phase,actor,capability,capability_revision,owner_digest
             FROM capability_invocation_audit ORDER BY rowid",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[tokio::test]
async fn production_gateway_allows_exact_tool_and_denies_unknown_or_forged_owner() {
    let directory = tempfile::tempdir().unwrap();
    let runtime_settings = settings(directory.path(), 41);
    let profiles = UserProfileStore::open(directory.path().join("profiles.db"))
        .await
        .unwrap();
    let factory = WorkerToolGatewayFactory::open(&runtime_settings, now(), profiles)
        .await
        .unwrap();
    let gateway = factory
        .build(
            AgentId::new("agent-a"),
            vec![tool_descriptor("command", ToolInvocationClass::Terminal)],
        )
        .unwrap();
    let context = ToolContext::new(SessionContext::new("alice", "agent-a", "session-a"));
    let request = tool_request(
        &gateway,
        "call-allow",
        "command",
        Some(ToolInvocationClass::Terminal),
        &context,
        json!({"command": "true"}),
    );
    let revision = request.snapshot().revision().to_owned();
    gateway
        .authorize(request)
        .await
        .unwrap()
        .finish(ToolInvocationOutcome::Succeeded)
        .await
        .unwrap();

    let rows = durable_audit_rows(&runtime_settings.curation_path);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "authorized");
    assert_eq!(rows[1].0, "completed");
    assert!(rows.iter().all(|row| row.1 == "worker"));
    assert!(rows.iter().all(|row| row.2 == "worker.tool::command"));
    assert!(rows.iter().all(|row| row.3 == revision));
    assert!(rows.iter().all(|row| !row.4.contains("alice")));

    let forged_input = tool_request(
        &gateway,
        "call-forged-input",
        "command",
        Some(ToolInvocationClass::Terminal),
        &context,
        json!({"metadata": {"user_id": "mallory"}}),
    );
    assert!(matches!(
        gateway.authorize(forged_input).await,
        Err(ToolInvocationError::AccessDenied)
    ));

    let wrong_actor = ToolContext::new(SessionContext::new("alice", "agent-b", "session-a"));
    let forged_actor = tool_request(
        &gateway,
        "call-forged-actor",
        "command",
        Some(ToolInvocationClass::Terminal),
        &wrong_actor,
        json!({}),
    );
    assert!(matches!(
        gateway.authorize(forged_actor).await,
        Err(ToolInvocationError::AccessDenied)
    ));

    let unknown = tool_request(
        &gateway,
        "call-unknown",
        "browser",
        None,
        &context,
        json!({}),
    );
    assert!(matches!(
        gateway.authorize(unknown).await,
        Err(ToolInvocationError::Unavailable)
    ));
    assert_eq!(durable_audit_rows(&runtime_settings.curation_path).len(), 2);
}

#[tokio::test]
async fn do_not_learn_profile_blocks_memory_candidates_but_not_other_tools() {
    let directory = tempfile::tempdir().unwrap();
    let runtime_settings = settings(directory.path(), 42);
    let profiles = UserProfileStore::open(directory.path().join("profiles.db"))
        .await
        .unwrap();
    let owner = UserId::new("alice");
    let created = profiles
        .create(owner.clone(), UserProfileData::default())
        .await
        .unwrap();
    profiles
        .set_do_not_learn(owner, created.revision, true)
        .await
        .unwrap();
    let factory = WorkerToolGatewayFactory::open(&runtime_settings, now(), profiles)
        .await
        .unwrap();
    let gateway = factory
        .build(
            AgentId::new("agent-a"),
            vec![
                tool_descriptor("memory_write", ToolInvocationClass::MemoryCandidate),
                tool_descriptor("read", ToolInvocationClass::Read),
            ],
        )
        .unwrap();
    let context = ToolContext::new(SessionContext::new("alice", "agent-a", "session-a"));

    let candidate = tool_request(
        &gateway,
        "call-memory",
        "memory_write",
        Some(ToolInvocationClass::MemoryCandidate),
        &context,
        json!({"content": "private preference"}),
    );
    assert!(matches!(
        gateway.authorize(candidate).await,
        Err(ToolInvocationError::AccessDenied)
    ));

    gateway
        .authorize(tool_request(
            &gateway,
            "call-read",
            "read",
            Some(ToolInvocationClass::Read),
            &context,
            json!({"path": "README.md"}),
        ))
        .await
        .unwrap()
        .finish(ToolInvocationOutcome::Succeeded)
        .await
        .unwrap();
    let rows = durable_audit_rows(&runtime_settings.curation_path);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.2 == "worker.tool::read"));
}

struct FixedLearningPreference(Result<bool, ()>);

#[async_trait]
impl LearningPreferenceSource for FixedLearningPreference {
    async fn do_not_learn(&self, _owner: &UserId) -> Result<bool, ()> {
        self.0
    }
}

#[derive(Default)]
struct SequencedAudit {
    failures: Mutex<VecDeque<bool>>,
    records: Mutex<Vec<CapabilityAuditRecord>>,
}

impl CapabilityAuditSink for SequencedAudit {
    fn record(&self, record: &CapabilityAuditRecord) -> Result<(), ()> {
        if self.failures.lock().unwrap().pop_front().unwrap_or(false) {
            return Err(());
        }
        self.records.lock().unwrap().push(record.clone());
        Ok(())
    }
}

fn test_gateway(
    descriptors: Vec<ToolInvocationDescriptor>,
    preferences: Result<bool, ()>,
    audit: Arc<dyn CapabilityAuditSink>,
) -> Arc<dyn ToolInvocationGateway> {
    build_worker_tool_gateway(
        AgentId::new("agent-a"),
        descriptors,
        Arc::new(FixedLearningPreference(preferences)),
        GuardianServiceIdentity::issue("guardian.runtime", 1, now() + 3_600).unwrap(),
        1,
        audit,
    )
    .unwrap()
}

#[tokio::test]
async fn unavailable_profile_store_denies_memory_candidate_without_sensitive_error_or_audit() {
    let audit = Arc::new(SequencedAudit::default());
    let gateway = test_gateway(
        vec![tool_descriptor(
            "memory_write",
            ToolInvocationClass::MemoryCandidate,
        )],
        Err(()),
        audit.clone(),
    );
    let context = ToolContext::new(SessionContext::new(
        "sensitive-owner",
        "agent-a",
        "session-a",
    ));
    let error = gateway
        .authorize(tool_request(
            &gateway,
            "call-profile-failure",
            "memory_write",
            Some(ToolInvocationClass::MemoryCandidate),
            &context,
            json!({"content": "sensitive-memory"}),
        ))
        .await
        .err()
        .unwrap();

    assert_eq!(error, ToolInvocationError::AccessDenied);
    assert_eq!(error.to_string(), "tool capability access denied");
    assert!(!error.to_string().contains("sensitive"));
    assert!(audit.records.lock().unwrap().is_empty());
}

#[tokio::test]
async fn production_gateway_maps_pre_and_terminal_audit_failures_without_replay() {
    let context = ToolContext::new(SessionContext::new("alice", "agent-a", "session-a"));

    let pre_audit = Arc::new(SequencedAudit::default());
    pre_audit.failures.lock().unwrap().push_back(true);
    let gateway = test_gateway(
        vec![tool_descriptor("command", ToolInvocationClass::Terminal)],
        Ok(false),
        pre_audit.clone(),
    );
    assert!(matches!(
        gateway
            .authorize(tool_request(
                &gateway,
                "call-pre-audit",
                "command",
                Some(ToolInvocationClass::Terminal),
                &context,
                json!({}),
            ))
            .await,
        Err(ToolInvocationError::AuditUnavailable)
    ));
    assert!(pre_audit.records.lock().unwrap().is_empty());

    let terminal_audit = Arc::new(SequencedAudit::default());
    terminal_audit
        .failures
        .lock()
        .unwrap()
        .extend([false, true]);
    let gateway = test_gateway(
        vec![tool_descriptor("command", ToolInvocationClass::Terminal)],
        Ok(false),
        terminal_audit.clone(),
    );
    let grant = gateway
        .authorize(tool_request(
            &gateway,
            "call-terminal-audit",
            "command",
            Some(ToolInvocationClass::Terminal),
            &context,
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        grant.finish(ToolInvocationOutcome::Succeeded).await,
        Err(ToolInvocationError::ExecutionOutcomeUncertain)
    );
    assert_eq!(terminal_audit.records.lock().unwrap().len(), 1);

    let cancellation_audit = Arc::new(SequencedAudit::default());
    let gateway = test_gateway(
        vec![tool_descriptor("command", ToolInvocationClass::Terminal)],
        Ok(false),
        cancellation_audit.clone(),
    );
    let grant = gateway
        .authorize(tool_request(
            &gateway,
            "call-cancelled",
            "command",
            Some(ToolInvocationClass::Terminal),
            &context,
            json!({}),
        ))
        .await
        .unwrap();
    drop(grant);
    let records = cancellation_audit.records.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].outcome, CapabilityAuditOutcome::Failed);
}

fn canonical_count(runtime: &GuardianRuntime) -> i64 {
    let connection = runtime.inner.canonical.connection.lock().unwrap();
    connection
        .query_row(
            "SELECT COUNT(*) FROM guardian_canonical_memory WHERE deleted=0",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn outbox_state(directory: &std::path::Path, event_id: &str) -> Option<String> {
    Connection::open(directory.join("curation.db"))
        .unwrap()
        .query_row(
            "SELECT state FROM guardian_outbox WHERE event_id=?1",
            [event_id],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}
