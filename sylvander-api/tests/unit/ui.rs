use super::{UiClientMessage, UiServerMessage};
use crate::{PermissionProfile, ReasoningEffort};

#[test]
fn minimal_chat_uses_current_optional_defaults() {
    let message: UiClientMessage =
        serde_json::from_str(r#"{"type":"chat","text":"hello"}"#).unwrap();
    assert!(matches!(
        message,
        UiClientMessage::Chat {
            attachments,
            session_id: None,
            workspace: None,
            ..
        } if attachments.is_empty()
    ));
}

#[test]
fn model_selection_requires_a_provider_qualified_wire_shape() {
    assert!(
        serde_json::from_str::<UiClientMessage>(
            r#"{"type":"select_model","model":"m","reasoning_effort":"off"}"#
        )
        .is_err()
    );
    let qualified = UiClientMessage::SelectModel {
        session_id: Some("session-1".into()),
        model: crate::ModelSelection {
            provider_id: "openai".into(),
            model_id: "gpt-5".into(),
        },
        reasoning_effort: ReasoningEffort::High,
    };
    let value = serde_json::to_value(&qualified).unwrap();
    assert_eq!(value["session_id"], "session-1");
    assert_eq!(value["model"]["provider_id"], "openai");
    assert_eq!(value["model"]["model_id"], "gpt-5");
    assert_eq!(
        serde_json::from_value::<UiClientMessage>(value).unwrap(),
        qualified
    );
}

#[test]
fn runtime_info_requires_a_provider_qualified_model() {
    assert!(
        serde_json::from_value::<UiServerMessage>(serde_json::json!({
            "type": "runtime_info",
            "model": "shared",
            "capabilities": 0,
            "approval_enabled": false,
            "max_attachment_bytes": 1024
        }))
        .is_err()
    );

    let qualified = UiServerMessage::RuntimeInfo {
        model: crate::ModelSelection {
            provider_id: "openai".into(),
            model_id: "shared".into(),
        },
        reasoning_effort: ReasoningEffort::Off,
        models: Vec::new(),
        permissions: PermissionProfile::default(),
        capabilities: 0,
        approval_enabled: false,
        max_attachment_bytes: 1024,
        platform: crate::PlatformSnapshot::default(),
    };
    let value = serde_json::to_value(qualified).unwrap();
    assert_eq!(value["model"]["provider_id"], "openai");
    assert_eq!(value["model"]["model_id"], "shared");
}

#[test]
fn model_selection_schema_exposes_only_the_qualified_input() {
    let schema = serde_json::to_string(&crate::schema::ui_protocol_schema()).unwrap();
    assert!(schema.contains("ModelSelection"));
    assert!(!schema.contains("ModelSelectionInput"));
    assert!(!schema.contains("model_selection"));
}

#[test]
fn selection_permissions_wire_keeps_session_identity() {
    let value = serde_json::to_value(UiClientMessage::SelectPermissions {
        session_id: Some("session-1".into()),
        profile: crate::PermissionProfile::default(),
    })
    .unwrap();
    assert_eq!(value["session_id"], "session-1");
}

#[test]
fn agent_administration_uses_one_transport_envelope() {
    let client: UiClientMessage = serde_json::from_value(serde_json::json!({
        "type": "agent_admin",
        "request": {
            "operation": "activate_revision",
            "agent_id": "oraculo",
            "revision": 5,
            "expected_active_revision": 4
        }
    }))
    .unwrap();
    assert!(matches!(
        client,
        UiClientMessage::AgentAdmin {
            request: crate::AgentAdminRequest::ActivateRevision {
                revision: 5,
                expected_active_revision: 4,
                ..
            }
        }
    ));

    let server = UiServerMessage::AgentAdmin {
        response: crate::AgentAdminResponse::Error {
            error: crate::AgentAdminError {
                code: crate::AgentAdminErrorCode::RevisionConflict,
                message: "active revision changed".into(),
                agent_id: Some(crate::AgentId::new("oraculo")),
                revision: Some(5),
                expected_active_revision: Some(4),
                actual_active_revision: Some(6),
            },
        },
    };
    let json = serde_json::to_value(server).unwrap();
    assert_eq!(json["type"], "agent_admin");
    assert_eq!(json["response"]["error"]["code"], "revision_conflict");
}

#[test]
fn registry_administration_uses_one_transport_envelope() {
    let client = UiClientMessage::RegistryAdmin {
        request: crate::RegistryAdminRequest::InspectProviderRevision {
            provider_id: "alpha".into(),
            revision: 2,
        },
    };
    let client_json = serde_json::to_value(&client).unwrap();
    assert_eq!(client_json["type"], "registry_admin");
    assert_eq!(
        serde_json::from_value::<UiClientMessage>(client_json).unwrap(),
        client
    );

    let server = UiServerMessage::RegistryAdmin {
        response: crate::RegistryAdminResponse::Error {
            error: crate::RegistryAdminError {
                code: crate::RegistryAdminErrorCode::StorageUnavailable,
                message: "registry unavailable".into(),
                provider_id: None,
                model_id: None,
                binding_id_sha256: None,
                revision: None,
                generation: None,
                details: None,
            },
        },
    };
    let server_json = serde_json::to_value(&server).unwrap();
    assert_eq!(server_json["type"], "registry_admin");
    assert_eq!(
        serde_json::from_value::<UiServerMessage>(server_json).unwrap(),
        server
    );
}

#[test]
fn user_profile_uses_one_strict_owner_free_transport_envelope() {
    let client: UiClientMessage = serde_json::from_value(serde_json::json!({
        "type": "user_profile",
        "request": {
            "version": 1,
            "action": {"operation": "read"}
        }
    }))
    .unwrap();
    assert!(matches!(
        client,
        UiClientMessage::UserProfile {
            request: crate::UserProfileRequest {
                action: crate::UserProfileAction::Read {},
                ..
            }
        }
    ));

    for owner_field in ["user_id", "owner_user_id"] {
        let mut value = serde_json::json!({
            "type": "user_profile",
            "request": {
                "version": 1,
                "action": {"operation": "read"}
            }
        });
        value[owner_field] = serde_json::json!("attacker-selected");
        assert!(serde_json::from_value::<UiClientMessage>(value).is_err());
    }

    let server = UiServerMessage::UserProfile {
        response: crate::UserProfileResponse::NotFound { version: 1 },
    };
    let json = serde_json::to_value(&server).unwrap();
    assert_eq!(json["type"], "user_profile");
    assert_eq!(
        serde_json::from_value::<UiServerMessage>(json).unwrap(),
        server
    );
}

#[test]
fn identity_binding_reuses_the_strict_owner_free_subprotocol() {
    let client: UiClientMessage = serde_json::from_value(serde_json::json!({
        "type": "identity_binding",
        "request": {
            "version": 1,
            "action": {"operation": "resolve"}
        }
    }))
    .unwrap();
    assert!(matches!(
        client,
        UiClientMessage::IdentityBinding { request }
            if matches!(request.action, crate::IdentityBindingAction::Resolve {})
    ));

    let server = UiServerMessage::IdentityBinding {
        response: std::sync::Arc::new(crate::IdentityBindingResponse::NotLinked { version: 1 }),
    };
    let json = serde_json::to_value(server).unwrap();
    assert_eq!(json["type"], "identity_binding");
    assert_eq!(json["response"]["result"], "not_linked");
}

#[test]
fn mutated_client_frames_are_total_and_strict_shapes_fail_closed() {
    let valid = br#"{"type":"chat","text":"hello","session_id":"session-1"}"#;
    let strict_failures = [
        Vec::new(),
        b"null".to_vec(),
        b"[]".to_vec(),
        b"{}".to_vec(),
        b"{".to_vec(),
        vec![0xff, 0xfe, 0xfd],
        br#"{"type":"chat","text":"hello","unknown":true}"#.to_vec(),
        br#"{"type":"unknown","text":"hello"}"#.to_vec(),
    ];
    for frame in strict_failures {
        assert!(
            serde_json::from_slice::<UiClientMessage>(&frame).is_err(),
            "invalid shape unexpectedly decoded: {frame:?}"
        );
    }

    let mut corpus = Vec::new();
    for index in 0..valid.len() {
        let mut deleted = valid.to_vec();
        deleted.remove(index);
        corpus.push(deleted);

        let mut replaced = valid.to_vec();
        replaced[index] = 0xff;
        corpus.push(replaced);
    }

    for frame in corpus {
        let parsed = std::panic::catch_unwind(|| serde_json::from_slice::<UiClientMessage>(&frame));
        assert!(parsed.is_ok(), "parser panicked for {frame:?}");
    }
}
