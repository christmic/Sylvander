use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::agent_definition::{AgentId, SessionId};
use crate::observability::{
    DEBUG_OBSERVATION_LOG_MAX_FILES, DEBUG_OBSERVATION_LOG_TOTAL_MAX_BYTES, RuntimeClock,
    RuntimeCoordinationOutcome, RuntimeDurationHistogramSnapshot, RuntimeEvent, RuntimeFailureKind,
    RuntimeObservability, RuntimeObservabilitySnapshot, RuntimeObservationDebugLog,
    RuntimePersistenceOperation, RuntimeToolFailureKind,
};
use sylvander_api::MessageId;

#[derive(Default)]
struct TestClock(AtomicU64);

impl TestClock {
    fn set(&self, micros: u64) {
        self.0.store(micros, Ordering::Relaxed);
    }
}

#[test]
fn coordination_facts_reduce_to_low_cardinality_counters() {
    let recorder = RuntimeObservability::with_test_clock(Arc::new(TestClock::default()));
    let session_id = SessionId::new("session-1");
    for outcome in [
        RuntimeCoordinationOutcome::Enqueued,
        RuntimeCoordinationOutcome::TaskCreated,
        RuntimeCoordinationOutcome::TaskTransitioned,
        RuntimeCoordinationOutcome::TaskClaimed,
        RuntimeCoordinationOutcome::TaskLeaseRecovered,
        RuntimeCoordinationOutcome::TaskCancelled,
        RuntimeCoordinationOutcome::BackgroundDispatchRecovered,
        RuntimeCoordinationOutcome::ArbitrationRequired,
        RuntimeCoordinationOutcome::ModeratorAuthorized,
        RuntimeCoordinationOutcome::ModeratorRejected,
        RuntimeCoordinationOutcome::ArbitrationApplied,
        RuntimeCoordinationOutcome::MailboxEscalated,
        RuntimeCoordinationOutcome::WorkspaceReviewPrepared,
        RuntimeCoordinationOutcome::WorkspaceApproved,
        RuntimeCoordinationOutcome::WorkspaceApplied,
        RuntimeCoordinationOutcome::WorkspaceMergeRecovered,
        RuntimeCoordinationOutcome::WorkspaceConflicted,
    ] {
        recorder.record(RuntimeEvent::CoordinationTransition {
            session_id: session_id.clone(),
            outcome,
        });
    }
    assert_eq!(
        recorder.snapshot(),
        RuntimeObservabilitySnapshot {
            event_count: 17,
            coordination_enqueued: 1,
            coordination_tasks_created: 1,
            coordination_tasks_transitioned: 1,
            coordination_tasks_claimed: 1,
            coordination_task_leases_recovered: 1,
            coordination_tasks_cancelled: 1,
            coordination_background_dispatches_recovered: 1,
            coordination_arbitration_required: 1,
            coordination_moderator_authorized: 1,
            coordination_moderator_rejected: 1,
            coordination_arbitration_applied: 1,
            coordination_mailbox_escalated: 1,
            workspace_reviews_prepared: 1,
            workspace_integrations_approved: 1,
            workspace_integrations_applied: 1,
            workspace_merges_recovered: 1,
            workspace_integrations_conflicted: 1,
            ..RuntimeObservabilitySnapshot::default()
        }
    );
}

impl RuntimeClock for TestClock {
    fn now_micros(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[test]
fn cloned_recorders_share_typed_lifecycle_counters() {
    let recorder = RuntimeObservability::with_test_clock(Arc::new(TestClock::default()));
    let ingress = recorder.clone();
    let session_id = SessionId::new("session-1");

    ingress.record(RuntimeEvent::chat_admitted(
        "request-1".into(),
        session_id.clone(),
        MessageId::new(),
        AgentId::new("agent-1"),
    ));
    ingress.record(RuntimeEvent::chat_dispatch_finished(
        "request-1".into(),
        session_id.clone(),
        true,
    ));
    ingress.record(RuntimeEvent::chat_admitted(
        "request-2".into(),
        session_id.clone(),
        MessageId::new(),
        AgentId::new("agent-1"),
    ));
    ingress.record(RuntimeEvent::chat_dispatch_finished(
        "request-2".into(),
        session_id,
        false,
    ));

    assert_eq!(
        recorder.snapshot(),
        RuntimeObservabilitySnapshot {
            event_count: 4,
            chat_admitted: 2,
            chat_dispatched: 1,
            chat_dispatch_failed: 1,
            dispatch_latency: RuntimeDurationHistogramSnapshot {
                count: 2,
                bucket_counts: [2, 0, 0, 0, 0, 0, 0, 0],
                ..RuntimeDurationHistogramSnapshot::default()
            },
            ..RuntimeObservabilitySnapshot::default()
        }
    );
}

#[tokio::test]
async fn governance_bus_is_ordered_and_shared_across_clones() {
    let recorder = RuntimeObservability::with_test_clock(Arc::new(TestClock::default()));
    let mut receiver = recorder.subscribe();
    let publisher = recorder.clone();
    let session_id = SessionId::new("session-1");
    publisher.record(RuntimeEvent::TurnStarted {
        request_id: "request-1".into(),
        trace_id: "trace-1".into(),
        turn_id: "turn-1".into(),
        session_id: session_id.clone(),
        agent_id: AgentId::new("agent-1"),
    });
    publisher.record(RuntimeEvent::TurnCompleted {
        turn_id: "turn-1".into(),
        session_id,
    });

    assert!(matches!(
        receiver.recv().await.unwrap(),
        RuntimeEvent::TurnStarted { .. }
    ));
    assert!(matches!(
        receiver.recv().await.unwrap(),
        RuntimeEvent::TurnCompleted { .. }
    ));
    assert_eq!(recorder.snapshot().turns_completed, 1);
}

#[tokio::test]
async fn debug_projection_writes_bounded_typed_jsonl_without_agent_content() {
    let directory = tempfile::TempDir::new().unwrap();
    let recorder = RuntimeObservability::with_test_clock(Arc::new(TestClock::default()));
    let debug_log = RuntimeObservationDebugLog::start(directory.path(), recorder.subscribe())
        .await
        .unwrap();
    let path = debug_log.path().to_path_buf();
    recorder.record(RuntimeEvent::TurnStarted {
        request_id: "request-1".into(),
        trace_id: "trace-1".into(),
        turn_id: "turn-1".into(),
        session_id: SessionId::new("session-1"),
        agent_id: AgentId::new("agent-1"),
    });
    debug_log.shutdown().await;

    let content = tokio::fs::read_to_string(path).await.unwrap();
    let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(record["event"], "turn_started");
    assert_eq!(record["turn_id"], "turn-1");
    assert!(record.get("recorded_at_unix_ms").is_some());
    assert!(!content.contains("prompt"));
    assert!(!content.contains("output"));
}

#[tokio::test]
async fn debug_projection_prunes_only_its_oldest_managed_files_across_restarts() {
    let directory = tempfile::TempDir::new().unwrap();
    let debug_directory = directory.path().join("debug");
    tokio::fs::create_dir_all(&debug_directory).await.unwrap();
    let mut old_paths = Vec::new();
    for sequence in 1..=DEBUG_OBSERVATION_LOG_MAX_FILES + 2 {
        let path = debug_directory.join(format!(
            "runtime-observations-00000000-0000-0000-0000-{sequence:012}.jsonl"
        ));
        tokio::fs::write(&path, b"old\n").await.unwrap();
        old_paths.push(path);
    }
    let unrelated = debug_directory.join("runtime-observations-manual-notes.jsonl");
    tokio::fs::write(&unrelated, b"keep\n").await.unwrap();

    let recorder = RuntimeObservability::with_test_clock(Arc::new(TestClock::default()));
    let debug_log = RuntimeObservationDebugLog::start(directory.path(), recorder.subscribe())
        .await
        .unwrap();
    let current = debug_log.path().to_path_buf();
    debug_log.shutdown().await;

    let retained_old = old_paths.iter().filter(|path| path.exists()).count();
    assert!(retained_old < DEBUG_OBSERVATION_LOG_MAX_FILES);
    assert!(retained_old + usize::from(current.exists()) <= DEBUG_OBSERVATION_LOG_MAX_FILES);
    assert!(unrelated.exists());
}

#[tokio::test]
async fn debug_projection_removes_oversized_managed_history_before_start() {
    let directory = tempfile::TempDir::new().unwrap();
    let debug_directory = directory.path().join("debug");
    tokio::fs::create_dir_all(&debug_directory).await.unwrap();
    let oversized =
        debug_directory.join("runtime-observations-00000000-0000-0000-0000-000000000001.jsonl");
    let file = tokio::fs::File::create(&oversized).await.unwrap();
    file.set_len(DEBUG_OBSERVATION_LOG_TOTAL_MAX_BYTES)
        .await
        .unwrap();
    drop(file);

    let recorder = RuntimeObservability::with_test_clock(Arc::new(TestClock::default()));
    let debug_log = RuntimeObservationDebugLog::start(directory.path(), recorder.subscribe())
        .await
        .unwrap();
    debug_log.shutdown().await;

    assert!(!oversized.exists());
}

#[test]
fn turn_tool_and_persistence_facts_reduce_to_content_safe_counts() {
    let recorder = RuntimeObservability::with_test_clock(Arc::new(TestClock::default()));
    let session_id = SessionId::new("session-1");
    let turn_id = "turn-1".to_owned();
    recorder.record(RuntimeEvent::TurnStarted {
        request_id: "request-1".into(),
        trace_id: "trace-1".into(),
        turn_id: turn_id.clone(),
        session_id: session_id.clone(),
        agent_id: AgentId::new("agent-1"),
    });
    recorder.record(RuntimeEvent::ModelRetried {
        turn_id: turn_id.clone(),
        session_id: session_id.clone(),
        attempt: 2,
    });
    recorder.record(RuntimeEvent::ToolStarted {
        turn_id: turn_id.clone(),
        session_id: session_id.clone(),
        tool_call_id: "call-1".into(),
        tool_name: "read".into(),
    });
    for (succeeded, failure_kind) in [
        (true, None),
        (
            false,
            Some(RuntimeToolFailureKind::FilesystemBoundaryPolicyViolation),
        ),
    ] {
        recorder.record(RuntimeEvent::ToolFinished {
            turn_id: turn_id.clone(),
            session_id: session_id.clone(),
            tool_call_id: "call-1".into(),
            tool_name: "read".into(),
            succeeded,
            failure_kind,
        });
    }
    for succeeded in [true, false] {
        recorder.record(RuntimeEvent::PersistenceFinished {
            turn_id: turn_id.clone(),
            session_id: session_id.clone(),
            operation: RuntimePersistenceOperation::BeginTurn,
            succeeded,
        });
    }
    recorder.record(RuntimeEvent::TurnCompleted {
        turn_id: turn_id.clone(),
        session_id: session_id.clone(),
    });
    recorder.record(RuntimeEvent::TurnInterrupted {
        turn_id: turn_id.clone(),
        session_id: session_id.clone(),
    });
    recorder.record(RuntimeEvent::TurnFailed {
        turn_id,
        session_id,
        kind: RuntimeFailureKind::AgentLoop,
    });

    assert_eq!(
        recorder.snapshot(),
        RuntimeObservabilitySnapshot {
            event_count: 10,
            turns_started: 1,
            turns_completed: 1,
            turns_interrupted: 1,
            turns_failed: 1,
            model_retries: 1,
            tools_started: 1,
            tools_succeeded: 1,
            tools_failed: 1,
            filesystem_policy_violations: 1,
            persistence_succeeded: 1,
            persistence_failed: 1,
            unmatched_terminals: 3,
            turn_latency: RuntimeDurationHistogramSnapshot {
                count: 1,
                bucket_counts: [1, 0, 0, 0, 0, 0, 0, 0],
                ..RuntimeDurationHistogramSnapshot::default()
            },
            tool_latency: RuntimeDurationHistogramSnapshot {
                count: 1,
                bucket_counts: [1, 0, 0, 0, 0, 0, 0, 0],
                ..RuntimeDurationHistogramSnapshot::default()
            },
            ..RuntimeObservabilitySnapshot::default()
        }
    );
}

#[test]
fn paired_lifecycles_report_active_work_and_bounded_latency() {
    let clock = Arc::new(TestClock::default());
    let recorder = RuntimeObservability::with_test_clock(clock.clone());
    let session_id = SessionId::new("session-1");
    recorder.record(RuntimeEvent::chat_admitted(
        "request-1".into(),
        session_id.clone(),
        MessageId::new(),
        AgentId::new("agent-1"),
    ));
    clock.set(20_000);
    recorder.record(RuntimeEvent::chat_dispatch_finished(
        "request-1".into(),
        session_id.clone(),
        true,
    ));
    recorder.record(RuntimeEvent::TurnStarted {
        request_id: "request-1".into(),
        trace_id: "trace-1".into(),
        turn_id: "turn-1".into(),
        session_id: session_id.clone(),
        agent_id: AgentId::new("agent-1"),
    });
    recorder.record(RuntimeEvent::ToolStarted {
        turn_id: "turn-1".into(),
        session_id: session_id.clone(),
        tool_call_id: "call-1".into(),
        tool_name: "read".into(),
    });
    let active = recorder.snapshot();
    assert_eq!(active.active_turns, 1);
    assert_eq!(active.active_tools, 1);

    clock.set(80_000);
    recorder.record(RuntimeEvent::ToolFinished {
        turn_id: "turn-1".into(),
        session_id: session_id.clone(),
        tool_call_id: "call-1".into(),
        tool_name: "read".into(),
        succeeded: true,
        failure_kind: None,
    });
    clock.set(600_000);
    recorder.record(RuntimeEvent::TurnCompleted {
        turn_id: "turn-1".into(),
        session_id,
    });

    let snapshot = recorder.snapshot();
    assert_eq!(snapshot.active_dispatches, 0);
    assert_eq!(snapshot.active_turns, 0);
    assert_eq!(snapshot.active_tools, 0);
    assert_eq!(snapshot.unmatched_terminals, 0);
    assert_eq!(
        snapshot.dispatch_latency,
        RuntimeDurationHistogramSnapshot {
            count: 1,
            total_micros: 20_000,
            max_micros: 20_000,
            bucket_counts: [0, 1, 0, 0, 0, 0, 0, 0],
        }
    );
    assert_eq!(snapshot.tool_latency.total_micros, 60_000);
    assert_eq!(
        snapshot.tool_latency.bucket_counts,
        [0, 0, 1, 0, 0, 0, 0, 0]
    );
    assert_eq!(snapshot.turn_latency.total_micros, 580_000);
    assert_eq!(
        snapshot.turn_latency.bucket_counts,
        [0, 0, 0, 0, 1, 0, 0, 0]
    );
}

#[test]
fn failed_turn_clears_unfinished_tool_timing_state() {
    let recorder = RuntimeObservability::with_test_clock(Arc::new(TestClock::default()));
    let session_id = SessionId::new("session-abandoned");
    recorder.record(RuntimeEvent::TurnStarted {
        request_id: "request-abandoned".into(),
        trace_id: "trace-abandoned".into(),
        turn_id: "turn-abandoned".into(),
        session_id: session_id.clone(),
        agent_id: AgentId::new("agent"),
    });
    recorder.record(RuntimeEvent::ToolStarted {
        turn_id: "turn-abandoned".into(),
        session_id: session_id.clone(),
        tool_call_id: "call-abandoned".into(),
        tool_name: "read".into(),
    });
    recorder.record(RuntimeEvent::TurnFailed {
        turn_id: "turn-abandoned".into(),
        session_id,
        kind: RuntimeFailureKind::Persistence,
    });

    let snapshot = recorder.snapshot();
    assert_eq!(snapshot.active_turns, 0);
    assert_eq!(snapshot.active_tools, 0);
}

#[test]
fn perception_evaluation_outcomes_are_counted_without_media_or_model_output() {
    let recorder = RuntimeObservability::new();
    let session_id = SessionId::new("session-perception");
    recorder.record(RuntimeEvent::PerceptionEvaluationFinished {
        turn_id: "turn-1".into(),
        session_id: session_id.clone(),
        invocation_id: "invocation-1".into(),
        succeeded: true,
        recovered_from_receipt: true,
        automatic: false,
    });
    recorder.record(RuntimeEvent::PerceptionEvaluationFinished {
        turn_id: "turn-2".into(),
        session_id,
        invocation_id: "invocation-2".into(),
        succeeded: false,
        recovered_from_receipt: false,
        automatic: true,
    });

    let snapshot = recorder.snapshot();
    assert_eq!(snapshot.perception_evaluations, 1);
    assert_eq!(snapshot.perception_evaluations_succeeded, 1);
    assert_eq!(snapshot.perception_evaluations_failed, 0);
    assert_eq!(snapshot.perception_automatic_routes, 1);
    assert_eq!(snapshot.perception_automatic_routes_succeeded, 0);
    assert_eq!(snapshot.perception_automatic_routes_soft_failed, 1);
    assert_eq!(snapshot.perception_receipts_recovered, 1);
}
