use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::agent_definition::{AgentId, SessionId};
use crate::observability::{
    RuntimeClock, RuntimeDurationHistogramSnapshot, RuntimeEvent, RuntimeFailureKind,
    RuntimeObservability, RuntimeObservabilitySnapshot, RuntimePersistenceOperation,
};
use sylvander_api::MessageId;

#[derive(Default)]
struct TestClock(AtomicU64);

impl TestClock {
    fn set(&self, micros: u64) {
        self.0.store(micros, Ordering::Relaxed);
    }
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
    for succeeded in [true, false] {
        recorder.record(RuntimeEvent::ToolFinished {
            turn_id: turn_id.clone(),
            session_id: session_id.clone(),
            tool_call_id: "call-1".into(),
            tool_name: "read".into(),
            succeeded,
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
