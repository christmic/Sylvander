use crate::agent_definition::{AgentId, SessionId};
use crate::observability::{RuntimeEvent, RuntimeObservability};
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

    let snapshot = recorder.snapshot();
    assert_eq!(snapshot.event_count, 2);
    assert_eq!(snapshot.chat_admitted, 1);
    assert_eq!(snapshot.chat_dispatched, 1);
}
