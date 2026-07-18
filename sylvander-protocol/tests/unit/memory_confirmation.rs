use super::*;

#[test]
fn latest_request_is_exact_and_owner_free() {
    let request = MemoryConfirmationRequest::Decide {
        version: MEMORY_CONFIRMATION_PROTOCOL_VERSION,
        session_id: "session-1".into(),
        candidate_id: "candidate-1".into(),
        expected_revision: 2,
        decision: MemoryConfirmationDecision::Confirm,
    };

    assert_eq!(request.operation(), "decide_memory_confirmation");
    assert_eq!(request.session_id(), "session-1");
    assert_eq!(request.validate(), Ok(()));
    let encoded = serde_json::to_value(request).unwrap();
    assert!(encoded.get("owner").is_none());
    assert!(encoded.get("user_id").is_none());
    assert!(encoded.get("agent_id").is_none());
}

#[test]
fn wrong_version_and_invalid_ids_fail_closed() {
    let wrong_version = MemoryConfirmationRequest::List {
        version: MEMORY_CONFIRMATION_PROTOCOL_VERSION + 1,
        session_id: "session-1".into(),
    };
    assert_eq!(
        wrong_version.validate(),
        Err(MemoryConfirmationValidationError::UnsupportedVersion)
    );

    let invalid = MemoryConfirmationRequest::Decide {
        version: MEMORY_CONFIRMATION_PROTOCOL_VERSION,
        session_id: " session-1".into(),
        candidate_id: "candidate-1".into(),
        expected_revision: 0,
        decision: MemoryConfirmationDecision::Reject,
    };
    assert_eq!(
        invalid.validate(),
        Err(MemoryConfirmationValidationError::InvalidRequest)
    );
}

#[test]
fn unknown_fields_are_rejected() {
    let encoded = serde_json::json!({
        "operation": "list",
        "version": 1,
        "session_id": "session-1",
        "owner": "attacker"
    });
    assert!(serde_json::from_value::<MemoryConfirmationRequest>(encoded).is_err());
}
