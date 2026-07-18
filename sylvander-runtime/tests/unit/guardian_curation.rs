use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use sylvander_protocol::{AgentId, SessionContext, UserId};

use crate::capability_runtime::{
    ActorCapabilityRuntime, AuthorizedCapabilityInvocation, CapabilityClass, CapabilityDefinition,
    CapabilityRegistry, RuntimeCapability, RuntimeOwnerScope, value_digest,
};

use super::*;

fn digest(seed: char) -> String {
    format!("sha256:{}", seed.to_string().repeat(64))
}

fn evidence(key: &str) -> Vec<EvidenceReference> {
    vec![EvidenceReference {
        kind: "run_event".into(),
        reference: key.into(),
        digest: digest('a'),
    }]
}

fn identity() -> GuardianServiceIdentity {
    GuardianServiceIdentity::issue("guardian.curator", 3, 100_000).unwrap()
}

fn owner(user: Option<&str>) -> RuntimeOwnerScope {
    RuntimeOwnerScope::guardian(
        AgentId::new("agent-a"),
        user.map(UserId::new),
        BTreeSet::from(["workspace-a".into()]),
    )
}

async fn open_store(path: &Path) -> GuardianCurationStore {
    GuardianCurationStore::open(path, identity(), 9)
        .await
        .unwrap()
}

async fn enqueue_and_claim(
    store: &GuardianCurationStore,
    event_id: &str,
    user: Option<&str>,
    now: i64,
) -> ClaimedCuratorRun {
    assert!(
        store
            .enqueue_event(
                GuardianEvent::new(
                    event_id,
                    GuardianEventKind::SessionClosed,
                    owner(user),
                    evidence("run:1"),
                    digest('b'),
                    now,
                ),
                now,
            )
            .await
            .unwrap()
    );
    store
        .claim_next_run(&identity(), "curator-v1", now, 900)
        .await
        .unwrap()
        .unwrap()
}

async fn classified_unique(
    store: &GuardianCurationStore,
    claim: &ClaimedCuratorRun,
    source_key: &str,
    scope: CandidateScope,
    sensitivity: Sensitivity,
    origin: CandidateOrigin,
    now: i64,
) -> MemoryCandidate {
    let extracted = store
        .extract_candidate(
            &identity(),
            claim,
            CandidateDraft {
                source_key: source_key.into(),
                content: json!({"fact": source_key}),
                evidence: evidence(source_key),
                origin,
            },
            now,
        )
        .await
        .unwrap();
    let classified = store
        .classify_candidate(
            &identity(),
            claim,
            &extracted.candidate_id,
            extracted.revision,
            CandidateClassification {
                scope,
                confidence_basis_points: 9_000,
                sensitivity,
                retention_secs: 30 * 24 * 60 * 60,
                dedupe_key: format!("dedupe:{source_key}"),
                workspace_id: matches!(scope, CandidateScope::WorkspaceKnowledge)
                    .then(|| "workspace-a".into()),
            },
            now + 1,
        )
        .await
        .unwrap();
    store
        .reconcile_candidate(
            &identity(),
            claim,
            &classified.candidate_id,
            classified.revision,
            Reconciliation::Unique,
            now + 2,
        )
        .await
        .unwrap()
}

async fn authorize_schedule(
    store: &GuardianCurationStore,
    claim: &ClaimedCuratorRun,
    candidate: &MemoryCandidate,
    now: i64,
) -> String {
    let decision = store
        .evaluate_policy(
            &identity(),
            claim,
            &candidate.candidate_id,
            candidate.revision,
            now,
        )
        .await
        .unwrap();
    assert_eq!(decision.outcome, PolicyOutcome::Allow);
    let authorized = store.candidate(&candidate.candidate_id).await.unwrap();
    store
        .schedule_mutation(
            &identity(),
            claim,
            &candidate.candidate_id,
            authorized.revision,
            now + 1,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn full_pipeline_is_durable_idempotent_and_restart_safe() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("curation.sqlite3");
    let store = open_store(&path).await;
    let claim = enqueue_and_claim(&store, "event-1", Some("alice"), 100).await;

    let candidate = classified_unique(
        &store,
        &claim,
        "candidate-0",
        CandidateScope::AgentCanonical,
        Sensitivity::Internal,
        CandidateOrigin::Explicit,
        110,
    )
    .await;
    assert_eq!(candidate.state, CandidateState::PolicyPending);

    let replay = store
        .extract_candidate(
            &identity(),
            &claim,
            CandidateDraft {
                source_key: "candidate-0".into(),
                content: json!({"fact": "candidate-0"}),
                evidence: evidence("candidate-0"),
                origin: CandidateOrigin::Explicit,
            },
            114,
        )
        .await
        .unwrap();
    assert_eq!(replay.candidate_id, candidate.candidate_id);

    let mutation_id = authorize_schedule(&store, &claim, &candidate, 115).await;
    let first_delivery = store
        .claim_next_mutation(&identity(), 117, 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_delivery.mutation_id, mutation_id);
    store
        .fail_mutation(
            &identity(),
            &first_delivery,
            "sink_temporarily_unavailable",
            118,
            Some(125),
        )
        .await
        .unwrap();
    assert_eq!(
        store.mutation_state(&mutation_id).await.unwrap(),
        MutationDeliveryState::Pending
    );
    assert!(
        store
            .claim_next_mutation(&identity(), 124, 60)
            .await
            .unwrap()
            .is_none()
    );
    let replayed_delivery = store
        .claim_next_mutation(&identity(), 125, 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        replayed_delivery.idempotency_key,
        first_delivery.idempotency_key
    );
    assert_eq!(replayed_delivery.attempt, 2);
    let committed = store
        .acknowledge_mutation(&identity(), &replayed_delivery, 126)
        .await
        .unwrap();
    assert_eq!(committed.state, CandidateState::Committed);
    assert_eq!(
        store.mutation_state(&mutation_id).await.unwrap(),
        MutationDeliveryState::Completed
    );

    store.finalize_run(&identity(), &claim, 127).await.unwrap();
    assert_eq!(
        store.run_state(&claim.run_id).await.unwrap(),
        CuratorRunState::Succeeded
    );
    drop(store);

    let reopened = open_store(&path).await;
    assert_eq!(
        reopened
            .candidate(&candidate.candidate_id)
            .await
            .unwrap()
            .state,
        CandidateState::Committed
    );
    assert!(
        reopened
            .claim_next_run(&identity(), "curator-v1", 200, 60)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn user_profile_requires_confirmation_and_secret_canonical_is_denied() {
    let directory = tempfile::tempdir().unwrap();
    let store = open_store(&directory.path().join("curation.sqlite3")).await;
    let claim = enqueue_and_claim(&store, "event-policy", Some("alice"), 1_000).await;

    let profile = classified_unique(
        &store,
        &claim,
        "profile",
        CandidateScope::UserProfile,
        Sensitivity::Personal,
        CandidateOrigin::Inferred,
        1_010,
    )
    .await;
    assert_eq!(profile.state, CandidateState::AwaitingConfirmation);
    assert_eq!(
        store
            .evaluate_policy(
                &identity(),
                &claim,
                &profile.candidate_id,
                profile.revision,
                1_013,
            )
            .await,
        Err(GuardianCurationError::Conflict)
    );
    let confirmed = store
        .confirm_candidate(
            &identity(),
            &claim,
            &profile.candidate_id,
            profile.revision,
            true,
            1_014,
        )
        .await
        .unwrap();
    let decision = store
        .evaluate_policy(
            &identity(),
            &claim,
            &confirmed.candidate_id,
            confirmed.revision,
            1_015,
        )
        .await
        .unwrap();
    assert_eq!(decision.outcome, PolicyOutcome::Allow);

    let secret = classified_unique(
        &store,
        &claim,
        "secret",
        CandidateScope::AgentCanonical,
        Sensitivity::Secret,
        CandidateOrigin::Explicit,
        1_020,
    )
    .await;
    let confirmed_secret = store
        .confirm_candidate(
            &identity(),
            &claim,
            &secret.candidate_id,
            secret.revision,
            true,
            1_024,
        )
        .await
        .unwrap();
    let denied = store
        .evaluate_policy(
            &identity(),
            &claim,
            &secret.candidate_id,
            confirmed_secret.revision,
            1_025,
        )
        .await
        .unwrap();
    assert_eq!(denied.outcome, PolicyOutcome::Deny);
    assert_eq!(denied.reason_code, "secret_storage_forbidden");
    assert_eq!(
        store.candidate(&secret.candidate_id).await.unwrap().state,
        CandidateState::Rejected
    );
}

#[tokio::test]
async fn duplicate_conflict_and_cross_owner_references_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let store = open_store(&directory.path().join("curation.sqlite3")).await;
    let claim = enqueue_and_claim(&store, "event-a", Some("alice"), 2_000).await;
    let existing = classified_unique(
        &store,
        &claim,
        "existing",
        CandidateScope::Relationship,
        Sensitivity::Internal,
        CandidateOrigin::Explicit,
        2_010,
    )
    .await;

    let duplicate = classified_unique(
        &store,
        &claim,
        "duplicate-seed",
        CandidateScope::Relationship,
        Sensitivity::Internal,
        CandidateOrigin::Explicit,
        2_020,
    )
    .await;
    // `classified_unique` already reconciles. Create a fresh classified item
    // to exercise explicit duplicate handling.
    let raw = store
        .extract_candidate(
            &identity(),
            &claim,
            CandidateDraft {
                source_key: "duplicate".into(),
                content: json!({"fact": "same"}),
                evidence: evidence("duplicate"),
                origin: CandidateOrigin::Explicit,
            },
            2_030,
        )
        .await
        .unwrap();
    let classified = store
        .classify_candidate(
            &identity(),
            &claim,
            &raw.candidate_id,
            raw.revision,
            CandidateClassification {
                scope: CandidateScope::Relationship,
                confidence_basis_points: 9_000,
                sensitivity: Sensitivity::Internal,
                retention_secs: 1_000,
                dedupe_key: "same".into(),
                workspace_id: None,
            },
            2_031,
        )
        .await
        .unwrap();
    let deduplicated = store
        .reconcile_candidate(
            &identity(),
            &claim,
            &classified.candidate_id,
            classified.revision,
            Reconciliation::DuplicateOf(existing.candidate_id.clone()),
            2_032,
        )
        .await
        .unwrap();
    assert_eq!(deduplicated.state, CandidateState::Duplicate);

    let other_claim = enqueue_and_claim(&store, "event-b", Some("bob"), 2_040).await;
    let other = classified_unique(
        &store,
        &other_claim,
        "other",
        CandidateScope::Relationship,
        Sensitivity::Internal,
        CandidateOrigin::Explicit,
        2_050,
    )
    .await;
    let conflict_raw = store
        .extract_candidate(
            &identity(),
            &claim,
            CandidateDraft {
                source_key: "conflict".into(),
                content: json!({"fact": "conflict"}),
                evidence: evidence("conflict"),
                origin: CandidateOrigin::Explicit,
            },
            2_060,
        )
        .await
        .unwrap();
    let conflict_classified = store
        .classify_candidate(
            &identity(),
            &claim,
            &conflict_raw.candidate_id,
            conflict_raw.revision,
            CandidateClassification {
                scope: CandidateScope::Relationship,
                confidence_basis_points: 8_000,
                sensitivity: Sensitivity::Internal,
                retention_secs: 1_000,
                dedupe_key: "conflict".into(),
                workspace_id: None,
            },
            2_061,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .reconcile_candidate(
                &identity(),
                &claim,
                &conflict_classified.candidate_id,
                conflict_classified.revision,
                Reconciliation::ConflictWith(other.candidate_id),
                2_062,
            )
            .await,
        Err(GuardianCurationError::AccessDenied)
    );
    assert_eq!(duplicate.state, CandidateState::PolicyPending);
}

#[tokio::test]
async fn expired_run_lease_is_reclaimed_and_stale_claim_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let store = open_store(&directory.path().join("curation.sqlite3")).await;
    assert!(
        store
            .enqueue_event(
                GuardianEvent::new(
                    "event-retry",
                    GuardianEventKind::UserFeedbackReceived,
                    owner(Some("alice")),
                    evidence("feedback:1"),
                    digest('c'),
                    3_000,
                ),
                3_000,
            )
            .await
            .unwrap()
    );
    let first = store
        .claim_next_run(&identity(), "curator-v1", 3_000, 10)
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .claim_next_run(&identity(), "curator-v1", 3_009, 10)
            .await
            .unwrap()
            .is_none()
    );
    let second = store
        .claim_next_run(&identity(), "curator-v1", 3_010, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.run_id, first.run_id);
    assert_eq!(second.attempt, 2);
    assert_eq!(
        store
            .extract_candidate(
                &identity(),
                &first,
                CandidateDraft {
                    source_key: "stale".into(),
                    content: json!({"stale": true}),
                    evidence: evidence("stale"),
                    origin: CandidateOrigin::Explicit,
                },
                3_011,
            )
            .await,
        Err(GuardianCurationError::LeaseLost)
    );
    store
        .retry_run(
            &identity(),
            &second,
            "transient_classifier_error",
            3_011,
            3_020,
        )
        .await
        .unwrap();
    assert_eq!(
        store.run_state(&second.run_id).await.unwrap(),
        CuratorRunState::Retryable
    );
    assert_eq!(
        store
            .claim_next_run(&identity(), "curator-v2", 3_020, 10)
            .await,
        Err(GuardianCurationError::Conflict)
    );
    let third = store
        .claim_next_run(&identity(), "curator-v1", 3_020, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(third.run_id, first.run_id);
    assert_eq!(third.attempt, 3);
}

#[tokio::test]
async fn correction_decay_and_forget_each_require_a_new_policy_decision() {
    let directory = tempfile::tempdir().unwrap();
    let store = open_store(&directory.path().join("curation.sqlite3")).await;
    let claim = enqueue_and_claim(&store, "event-lifecycle", Some("alice"), 4_000).await;
    let candidate = classified_unique(
        &store,
        &claim,
        "lifecycle",
        CandidateScope::Relationship,
        Sensitivity::Internal,
        CandidateOrigin::Explicit,
        4_010,
    )
    .await;

    authorize_schedule(&store, &claim, &candidate, 4_020).await;
    let delivery = store
        .claim_next_mutation(&identity(), 4_022, 60)
        .await
        .unwrap()
        .unwrap();
    let mut current = store
        .acknowledge_mutation(&identity(), &delivery, 4_023)
        .await
        .unwrap();
    assert_eq!(current.state, CandidateState::Committed);

    current = store
        .correct_candidate(
            &identity(),
            &claim,
            &current.candidate_id,
            current.revision,
            CandidateCorrection {
                content: json!({"fact": "corrected"}),
                evidence: evidence("correction"),
            },
            4_030,
        )
        .await
        .unwrap();
    authorize_schedule(&store, &claim, &current, 4_031).await;
    let delivery = store
        .claim_next_mutation(&identity(), 4_033, 60)
        .await
        .unwrap()
        .unwrap();
    current = store
        .acknowledge_mutation(&identity(), &delivery, 4_034)
        .await
        .unwrap();
    assert_eq!(current.state, CandidateState::Corrected);

    current = store
        .decay_candidate(
            &identity(),
            &claim,
            &current.candidate_id,
            current.revision,
            4_040,
        )
        .await
        .unwrap();
    authorize_schedule(&store, &claim, &current, 4_041).await;
    let delivery = store
        .claim_next_mutation(&identity(), 4_043, 60)
        .await
        .unwrap()
        .unwrap();
    current = store
        .acknowledge_mutation(&identity(), &delivery, 4_044)
        .await
        .unwrap();
    assert_eq!(current.state, CandidateState::Decayed);

    current = store
        .forget_candidate(
            &identity(),
            &claim,
            &current.candidate_id,
            current.revision,
            4_050,
        )
        .await
        .unwrap();
    authorize_schedule(&store, &claim, &current, 4_051).await;
    let delivery = store
        .claim_next_mutation(&identity(), 4_053, 60)
        .await
        .unwrap()
        .unwrap();
    current = store
        .acknowledge_mutation(&identity(), &delivery, 4_054)
        .await
        .unwrap();
    assert_eq!(current.state, CandidateState::Forgotten);
}

#[tokio::test]
async fn permanent_delivery_and_processing_failures_are_terminal_and_auditable() {
    let directory = tempfile::tempdir().unwrap();
    let store = open_store(&directory.path().join("curation.sqlite3")).await;
    let claim = enqueue_and_claim(&store, "event-dead-letter", Some("alice"), 4_500).await;
    let candidate = classified_unique(
        &store,
        &claim,
        "dead-letter",
        CandidateScope::Relationship,
        Sensitivity::Internal,
        CandidateOrigin::Explicit,
        4_510,
    )
    .await;
    let mutation_id = authorize_schedule(&store, &claim, &candidate, 4_520).await;
    let delivery = store
        .claim_next_mutation(&identity(), 4_522, 60)
        .await
        .unwrap()
        .unwrap();
    store
        .fail_mutation(
            &identity(),
            &delivery,
            "permanent_sink_rejection",
            4_523,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        store.mutation_state(&mutation_id).await.unwrap(),
        MutationDeliveryState::DeadLetter
    );
    assert_eq!(
        store
            .candidate(&candidate.candidate_id)
            .await
            .unwrap()
            .state,
        CandidateState::DeliveryFailed
    );
    store
        .finalize_run(&identity(), &claim, 4_524)
        .await
        .unwrap();
    assert_eq!(
        store.run_state(&claim.run_id).await.unwrap(),
        CuratorRunState::Failed
    );

    let failed_claim =
        enqueue_and_claim(&store, "event-processor-failed", Some("alice"), 4_530).await;
    store
        .fail_run(
            &identity(),
            &failed_claim,
            "unsupported_source_reference",
            4_531,
        )
        .await
        .unwrap();
    assert_eq!(
        store.run_state(&failed_claim.run_id).await.unwrap(),
        CuratorRunState::Failed
    );
}

#[tokio::test]
async fn latest_schema_fails_closed_and_store_durably_audits_capabilities() {
    let directory = tempfile::tempdir().unwrap();
    let incompatible = directory.path().join("old.sqlite3");
    let connection = rusqlite::Connection::open(&incompatible).unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    drop(connection);
    assert_eq!(
        GuardianCurationStore::open(&incompatible, identity(), 9)
            .await
            .err(),
        Some(GuardianCurationError::IncompatibleSchema)
    );

    let store = open_store(&directory.path().join("latest.sqlite3")).await;
    let calls = Arc::new(std::sync::Mutex::new(0_u32));
    let worker = CapabilityRegistry::new()
        .register(AuditedCapability {
            calls: calls.clone(),
        })
        .unwrap();
    let runtime = ActorCapabilityRuntime::new(
        worker,
        CapabilityRegistry::new(),
        identity(),
        9,
        Arc::new(store.clone()),
    )
    .unwrap();
    let snapshot = runtime.begin_worker_run(
        &SessionContext::new("alice", "agent-a", "session-a"),
        BTreeSet::new(),
    );
    assert_eq!(
        snapshot.invoke("read_memory", &json!({}), 5_000).await,
        Ok(json!({"ok": true}))
    );
    assert_eq!(*calls.lock().unwrap(), 1);
    let audit_count: i64 = store
        .connection
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM capability_invocation_audit",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(audit_count, 2);
}

struct AuditedCapability {
    calls: Arc<std::sync::Mutex<u32>>,
}

#[async_trait]
impl RuntimeCapability for AuditedCapability {
    fn definition(&self) -> CapabilityDefinition {
        let schema = json!({"type": "object"});
        CapabilityDefinition {
            name: "read_memory".into(),
            version: 1,
            class: CapabilityClass::Read,
            schema_digest: value_digest(&schema),
            schema,
        }
    }

    async fn invoke(
        &self,
        _invocation: AuthorizedCapabilityInvocation<'_>,
    ) -> Result<Value, crate::capability_runtime::CapabilityRuntimeError> {
        *self.calls.lock().unwrap() += 1;
        Ok(json!({"ok": true}))
    }
}
