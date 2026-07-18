use serde_json::json;

use super::*;

fn hello(capabilities: &[&str]) -> UiProtocolHello {
    UiProtocolHello {
        client_name: "test".into(),
        min_version: crate::UI_PROTOCOL_VERSION,
        max_version: crate::UI_PROTOCOL_VERSION,
        capabilities: capabilities.iter().map(ToString::to_string).collect(),
    }
}

fn welcome(capabilities: &[&str]) -> UiProtocolWelcome {
    UiProtocolWelcome {
        server_name: "test".into(),
        version: crate::UI_PROTOCOL_VERSION,
        capabilities: capabilities.iter().map(ToString::to_string).collect(),
    }
}

#[test]
fn request_never_accepts_transport_or_external_principal_fields() {
    let valid: IdentityBindingRequest = serde_json::from_value(json!({
        "version": 1,
        "action": {"operation": "begin"}
    }))
    .unwrap();
    assert_eq!(valid.operation(), IdentityBindingOperation::Begin);
    assert_eq!(valid.validate(), Ok(()));

    for forbidden in ["transport", "channel_instance_id", "external_principal"] {
        let mut value = json!({
            "version": 1,
            "action": {"operation": "resolve"}
        });
        value[forbidden] = json!("attacker-controlled");
        assert!(serde_json::from_value::<IdentityBindingRequest>(value).is_err());
    }
    assert!(
        serde_json::from_value::<IdentityBindingRequest>(json!({
            "version": 1,
            "action": {"operation": "begin", "target_user_id": "victim"}
        }))
        .is_err()
    );
}

#[test]
fn requests_are_exact_version_bounded_and_strict() {
    let unsupported: IdentityBindingRequest = serde_json::from_value(json!({
        "version": 2,
        "action": {"operation": "resolve"}
    }))
    .unwrap();
    assert_eq!(
        unsupported.validate(),
        Err(IdentityBindingValidationError::UnsupportedVersion)
    );

    assert!(
        serde_json::from_value::<IdentityBindingRequest>(json!({
            "version": 1,
            "action": {"operation": "resolve", "unexpected": true}
        }))
        .is_err()
    );
}

#[test]
fn challenge_secret_serializes_once_and_debug_is_redacted() {
    let raw = "link-secret-that-must-not-leak";
    let response = IdentityBindingResponse::ChallengeIssued {
        version: IDENTITY_BINDING_PROTOCOL_VERSION,
        challenge_id: IdentityLinkChallengeId::new("challenge-1").unwrap(),
        secret: OneTimeIdentityLinkSecret::new(raw).unwrap(),
        expires_at_unix_secs: 1_234,
    };

    let debug = format!("{response:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains(raw));

    let first = serde_json::to_string(&response).unwrap();
    assert!(first.contains(raw));
    assert!(serde_json::to_string(&response).is_err());
}

#[test]
fn secret_proof_debug_is_redacted_and_wire_validation_is_bounded() {
    let raw = "confirmation-secret-value";
    let request: IdentityBindingRequest = serde_json::from_value(json!({
        "version": 1,
        "action": {
            "operation": "confirm",
            "challenge_id": "challenge-1",
            "proof": raw
        }
    }))
    .unwrap();
    assert_eq!(request.validate(), Ok(()));
    assert!(!format!("{request:?}").contains(raw));

    assert!(
        serde_json::from_value::<IdentityBindingRequest>(json!({
            "version": 1,
            "action": {
                "operation": "confirm",
                "challenge_id": "challenge-1",
                "proof": "short"
            }
        }))
        .is_err()
    );
}

#[test]
fn ordinary_responses_and_errors_have_no_secret_slot() {
    let responses = [
        IdentityBindingResponse::NotLinked { version: 1 },
        IdentityBindingResponse::Unlinked { version: 1 },
        IdentityBindingResponse::Error {
            version: 1,
            error: IdentityBindingError::service_unavailable(IdentityBindingOperation::Confirm),
        },
    ];
    for response in responses {
        let value = serde_json::to_value(response).unwrap();
        assert!(value.get("secret").is_none());
        assert!(!value.to_string().contains("external_principal"));
    }
}

#[test]
fn capability_negotiation_requires_explicit_mutual_opt_in() {
    assert!(!IdentityBindingCapabilities::default().supports(1));
    assert!(IdentityBindingCapabilities::current().supports(1));
    assert!(!identity_binding_is_negotiated(&hello(&[]), &welcome(&[])));
    assert!(!identity_binding_is_negotiated(
        &hello(&[IDENTITY_BINDING_CAPABILITY]),
        &welcome(&[]),
    ));
    assert!(!identity_binding_is_negotiated(
        &hello(&[]),
        &welcome(&[IDENTITY_BINDING_CAPABILITY]),
    ));
    assert!(identity_binding_is_negotiated(
        &hello(&[IDENTITY_BINDING_CAPABILITY]),
        &welcome(&[IDENTITY_BINDING_CAPABILITY]),
    ));

    let mut invalid_welcome = welcome(&[IDENTITY_BINDING_CAPABILITY]);
    invalid_welcome.version = crate::UI_PROTOCOL_VERSION + 1;
    assert!(!identity_binding_is_negotiated(
        &hello(&[IDENTITY_BINDING_CAPABILITY]),
        &invalid_welcome,
    ));
}

#[test]
fn schema_exposes_actions_without_transport_identity_or_general_secret_views() {
    let schema = crate::schema::identity_binding_protocol_schema();
    let encoded = serde_json::to_string(&schema).unwrap();
    for action in ["begin", "confirm", "resolve", "unlink"] {
        assert!(encoded.contains(action), "schema omitted {action}");
    }
    assert!(!encoded.contains("external_principal"));
    assert!(!encoded.contains("channel_instance_id"));
    assert!(!encoded.contains("IdentityBindingView_secret"));
}
