use serde_json::json;

use super::*;

fn profile_value() -> serde_json::Value {
    json!({
        "preferred_language": {
            "value": "zh-CN",
            "privacy_class": "personal"
        },
        "locale": {
            "value": "zh_CN",
            "privacy_class": "sensitive"
        },
        "response_detail": {
            "value": "concise",
            "privacy_class": "personal"
        },
        "communication_tone": {
            "value": "direct",
            "privacy_class": "personal"
        },
        "accessibility": {
            "value": {
                "screen_reader_optimized": true,
                "reduce_motion": true,
                "high_contrast": false
            },
            "privacy_class": "restricted"
        },
        "constraints": [{
            "value": "不要将我的偏好推广到其他用户",
            "privacy_class": "restricted"
        }]
    })
}

fn request(operation: serde_json::Value) -> UserProfileRequest {
    serde_json::from_value(json!({"version": 1, "action": operation})).unwrap()
}

#[test]
fn every_operation_is_owner_free_strict_and_versioned() {
    let operations = [
        json!({"operation": "create", "profile": profile_value()}),
        json!({"operation": "read"}),
        json!({"operation": "update", "expected_revision": 1, "profile": profile_value()}),
        json!({"operation": "export", "format": "json"}),
        json!({"operation": "correct", "expected_revision": 1, "profile": profile_value()}),
        json!({"operation": "delete", "expected_revision": 1}),
        json!({"operation": "set_do_not_learn", "expected_revision": 1, "enabled": true}),
    ];

    for operation in operations {
        assert_eq!(request(operation).validate(), Ok(()));
    }

    for owner_field in ["user_id", "owner_user_id", "principal_id"] {
        let mut value = json!({"version": 1, "action": {"operation": "read"}});
        value[owner_field] = json!("victim");
        assert!(serde_json::from_value::<UserProfileRequest>(value).is_err());
    }
    assert!(
        serde_json::from_value::<UserProfileRequest>(json!({
            "version": 1,
            "action": {"operation": "read", "unexpected": true}
        }))
        .is_err()
    );
}

#[test]
fn unsupported_versions_and_zero_revisions_fail_closed() {
    let unsupported: UserProfileRequest = serde_json::from_value(json!({
        "version": 2,
        "action": {"operation": "read"}
    }))
    .unwrap();
    assert_eq!(
        unsupported.validate(),
        Err(UserProfileValidationError::UnsupportedVersion)
    );

    for operation in ["update", "correct"] {
        let invalid = request(json!({
            "operation": operation,
            "expected_revision": 0,
            "profile": profile_value()
        }));
        assert_eq!(
            invalid.validate(),
            Err(UserProfileValidationError::InvalidRevision)
        );
    }
    for operation in ["delete", "set_do_not_learn"] {
        let mut action = json!({"operation": operation, "expected_revision": 0});
        if operation == "set_do_not_learn" {
            action["enabled"] = json!(true);
        }
        assert_eq!(
            request(action).validate(),
            Err(UserProfileValidationError::InvalidRevision)
        );
    }
}

#[test]
fn text_and_collection_inputs_are_bounded_during_deserialization() {
    for invalid_language in ["", " zh-CN", "zh\nCN"] {
        let mut profile = profile_value();
        profile["preferred_language"]["value"] = json!(invalid_language);
        assert!(
            serde_json::from_value::<UserProfileRequest>(json!({
                "version": 1,
                "action": {"operation": "create", "profile": profile}
            }))
            .is_err()
        );
    }

    let mut profile = profile_value();
    profile["constraints"] = serde_json::Value::Array(
        (0..=MAX_CONSTRAINTS)
            .map(|_| json!({"value": "bounded", "privacy_class": "personal"}))
            .collect(),
    );
    assert!(
        serde_json::from_value::<UserProfileRequest>(json!({
            "version": 1,
            "action": {"operation": "create", "profile": profile}
        }))
        .is_err()
    );
}

#[test]
fn debug_never_exposes_profile_values() {
    let raw_language = "secret-language-marker";
    let raw_constraint = "secret-constraint-marker";
    let profile = UserProfileData {
        preferred_language: Some(ClassifiedPreference {
            value: LanguageTag::new(raw_language).unwrap(),
            privacy_class: PrivacyClass::Sensitive,
        }),
        constraints: vec![ClassifiedPreference {
            value: ProfileConstraint::new(raw_constraint).unwrap(),
            privacy_class: PrivacyClass::Restricted,
        }],
        ..UserProfileData::default()
    };
    let request = UserProfileRequest {
        version: 1,
        action: UserProfileAction::Create { profile },
    };

    let debug = format!("{request:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains(raw_language));
    assert!(!debug.contains(raw_constraint));
}

#[test]
fn response_contract_preserves_privacy_controls_without_owner_identity() {
    let response: UserProfileResponse = serde_json::from_value(json!({
        "version": 1,
        "result": "deleted",
        "deleted_revision": 4,
        "do_not_learn_preserved": true
    }))
    .unwrap();
    let encoded = serde_json::to_value(response).unwrap();
    assert_eq!(encoded["do_not_learn_preserved"], true);
    assert!(encoded.get("user_id").is_none());

    assert!(
        serde_json::from_value::<UserProfileResponse>(json!({
            "version": 1,
            "result": "not_found",
            "detail": "database table leaked"
        }))
        .is_err()
    );
}

#[test]
fn capability_requires_mutual_explicit_negotiation() {
    let hello = |capabilities: Vec<String>| UiProtocolHello {
        client_name: "test".into(),
        min_version: crate::UI_PROTOCOL_VERSION,
        max_version: crate::UI_PROTOCOL_VERSION,
        capabilities,
    };
    let welcome = |capabilities: Vec<String>| UiProtocolWelcome {
        server_name: "test".into(),
        version: crate::UI_PROTOCOL_VERSION,
        capabilities,
    };
    let capability = vec![USER_PROFILE_CAPABILITY.to_owned()];

    assert!(!UserProfileCapabilities::default().supports(1));
    assert!(UserProfileCapabilities::current().supports(1));
    assert!(!user_profile_is_negotiated(
        &hello(capability.clone()),
        &welcome(vec![])
    ));
    assert!(user_profile_is_negotiated(
        &hello(capability.clone()),
        &welcome(capability)
    ));
}

#[test]
fn schema_exposes_typed_rights_and_no_owner_selector() {
    let encoded = serde_json::to_string(&crate::schema::user_profile_protocol_schema()).unwrap();
    for value in [
        "create",
        "read",
        "update",
        "export",
        "correct",
        "delete",
        "set_do_not_learn",
        "PrivacyClass",
        "AccessibilityPreferences",
    ] {
        assert!(encoded.contains(value), "schema omitted {value}");
    }
    assert!(!encoded.contains("user_id"));
    assert!(!encoded.contains("owner_user_id"));
}
