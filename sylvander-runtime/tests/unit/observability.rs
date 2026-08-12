use crate::agent_definition::{AgentId, SessionId};
use crate::observability::{
    RuntimeEvent, RuntimeFailureKind, RuntimeObservability, RuntimeObservabilitySnapshot,
    RuntimePersistenceOperation,
};
use sylvander_api::MessageId;

#[test]
fn cloned_recorders_share_typed_lifecycle_counters() {
    let recorder = RuntimeObservability::new();
    let ingress = recorder.clone();
    let session_id = SessionId::new("session-1");

    ingress.record(RuntimeEvent::chat_admitted(
        "request-1".into(),
        session_id.clone(),
        MessageId::new(),
        AgentId::new("agent-1"),
    ));
    ingress.record(RuntimeEvent::chat_dispatched(
        "request-1".into(),
        session_id,
    ));

    assert_eq!(
        recorder.snapshot(),
        RuntimeObservabilitySnapshot {
            event_count: 2,
            chat_admitted: 1,
            chat_dispatched: 1,
            ..RuntimeObservabilitySnapshot::default()
        }
    );
}

#[test]
fn turn_tool_and_persistence_facts_reduce_to_content_safe_counts() {
    let recorder = RuntimeObservability::new();
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
            ..RuntimeObservabilitySnapshot::default()
        }
    );
}
