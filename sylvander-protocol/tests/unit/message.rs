use super::*;

#[test]
fn current_bus_messages_may_omit_an_empty_attachment_list() {
    let mut value =
        serde_json::to_value(BusMessage::user_chat("s".into(), "u", "hi")).expect("serialize");
    value.as_object_mut().unwrap().remove("attachments");
    let message: BusMessage = serde_json::from_value(value).expect("current optional field");
    assert!(message.attachments.is_empty());
}

#[test]
fn approval_messages_require_explicit_scope_contracts() {
    assert!(
        serde_json::from_value::<SystemMessage>(serde_json::json!({
            "type": "approve_tool",
            "call_id": "call-1",
            "approved": true
        }))
        .is_err()
    );

    assert!(
        serde_json::from_value::<StreamEvent>(serde_json::json!({
            "type": "tool_approval_required",
            "batch_id": "batch-1",
            "tools": []
        }))
        .is_err()
    );
}

#[test]
fn approval_rejection_reason_round_trips_without_transport_semantics() {
    let system = SystemMessage::ApproveTool {
        call_id: "call-1".into(),
        approved: false,
        scope: ApprovalScope::Once,
        reason: Some("unsafe outside workspace".into()),
    };
    let json = serde_json::to_value(&system).expect("serialize approval");
    let decoded: SystemMessage = serde_json::from_value(json).expect("decode approval");
    assert_eq!(decoded, system);
}

#[test]
fn retry_events_require_an_explicit_typed_cause() {
    assert!(
        serde_json::from_value::<StreamEvent>(serde_json::json!({
            "type": "model_retry",
            "attempt": 1,
            "max_attempts": 3,
            "delay_ms": 100,
            "reason": "temporary"
        }))
        .is_err()
    );
}

#[test]
fn terminal_error_has_a_stable_typed_wire_shape() {
    let event = StreamEvent::Error {
        message: "provider unavailable".into(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "error");
    assert_eq!(json["message"], "provider unavailable");
    assert_eq!(serde_json::from_value::<StreamEvent>(json).unwrap(), event);
}
