use serde_json::json;

use super::*;

fn view() -> ProviderRevisionView {
    ProviderRevisionView {
        definition: RedactedProviderDefinition {
            provider_id: "alpha".into(),
            revision: 2,
            kind: "anthropic_compatible".into(),
            base_url_sha256: "base-digest".into(),
            credential_binding_id_sha256: "binding-digest".into(),
        },
        digest_sha256: "definition-digest".into(),
        created_at_unix_secs: 7,
        active: true,
    }
}

fn model_view() -> ModelRevisionView {
    ModelRevisionView {
        definition: RedactedModelDefinition {
            provider_id: "alpha".into(),
            model_id: "model-a".into(),
            revision: 3,
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities: vec!["tool_use".into()],
            lifecycle: crate::ModelLifecycle::Active,
            pricing_sha256: Some("pricing-digest".into()),
        },
        digest_sha256: "model-digest".into(),
        created_at_unix_secs: 8,
        active: true,
    }
}

#[test]
fn list_defaults_and_round_trips() {
    let request: RegistryAdminRequest = serde_json::from_value(json!({
        "operation": "list_provider_revisions",
        "provider_id": "alpha"
    }))
    .unwrap();
    assert_eq!(
        request,
        RegistryAdminRequest::ListProviderRevisions {
            provider_id: "alpha".into(),
            before_revision: None,
            limit: DEFAULT_REGISTRY_REVISION_PAGE_SIZE,
        }
    );
    request.validate().unwrap();
    assert_eq!(
        serde_json::from_value::<RegistryAdminRequest>(serde_json::to_value(&request).unwrap())
            .unwrap(),
        request
    );
}

#[test]
fn validation_rejects_bad_identity_revision_and_page_limits() {
    for request in [
        RegistryAdminRequest::InspectProviderRevision {
            provider_id: " ".into(),
            revision: 1,
        },
        RegistryAdminRequest::InspectProviderRevision {
            provider_id: "alpha".into(),
            revision: 0,
        },
        RegistryAdminRequest::ListProviderRevisions {
            provider_id: "alpha".into(),
            before_revision: None,
            limit: 0,
        },
        RegistryAdminRequest::ListProviderRevisions {
            provider_id: "alpha".into(),
            before_revision: None,
            limit: 101,
        },
    ] {
        assert_eq!(
            request.validate().unwrap_err().code,
            RegistryAdminErrorCode::InvalidRequest
        );
    }
    assert!(
        serde_json::from_value::<RegistryAdminRequest>(json!({
            "operation": "list_provider_revisions",
            "provider_id": "alpha",
            "unexpected": true
        }))
        .is_err()
    );
}

#[test]
fn provider_response_exposes_digests_not_configured_values() {
    let response = RegistryAdminResponse::Success {
        result: Box::new(RegistryAdminResult::ProviderRevisionInspected { revision: view() }),
    };
    let encoded = serde_json::to_value(response).unwrap();
    let definition = &encoded["result"]["revision"]["definition"];
    assert_eq!(definition["base_url_sha256"], "base-digest");
    assert_eq!(definition["credential_binding_id_sha256"], "binding-digest");
    assert!(definition.get("base_url").is_none());
    assert!(definition.get("credential_binding_id").is_none());
}

#[test]
fn model_list_defaults_validates_and_round_trips() {
    let request: RegistryAdminRequest = serde_json::from_value(json!({
        "operation": "list_model_revisions",
        "provider_id": "alpha",
        "model_id": "model-a"
    }))
    .unwrap();
    assert_eq!(
        request,
        RegistryAdminRequest::ListModelRevisions {
            provider_id: "alpha".into(),
            model_id: "model-a".into(),
            before_revision: None,
            limit: DEFAULT_REGISTRY_REVISION_PAGE_SIZE,
        }
    );
    request.validate().unwrap();
    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<RegistryAdminRequest>(encoded).unwrap(),
        request
    );
    for invalid in [
        RegistryAdminRequest::InspectModelRevision {
            provider_id: "alpha".into(),
            model_id: " ".into(),
            revision: 1,
        },
        RegistryAdminRequest::ListModelRevisions {
            provider_id: "alpha".into(),
            model_id: "model-a".into(),
            before_revision: Some(0),
            limit: 50,
        },
    ] {
        assert_eq!(
            invalid.validate().unwrap_err().code,
            RegistryAdminErrorCode::InvalidRequest
        );
    }
}

#[test]
fn model_response_exposes_pricing_digest_not_pricing_configuration() {
    let response = RegistryAdminResponse::Success {
        result: Box::new(RegistryAdminResult::ModelRevisionInspected {
            revision: model_view(),
        }),
    };
    let encoded = serde_json::to_value(response).unwrap();
    let definition = &encoded["result"]["revision"]["definition"];
    assert_eq!(definition["pricing_sha256"], "pricing-digest");
    for field in [
        "pricing",
        "input_usd_micros_per_million",
        "output_usd_micros_per_million",
    ] {
        assert!(definition.get(field).is_none());
    }
}

#[test]
fn model_error_identity_is_typed_and_legacy_errors_default_it() {
    let current: RegistryAdminError = serde_json::from_value(json!({
        "code": "unknown_model",
        "message": "model is unknown",
        "provider_id": "alpha",
        "model_id": "model-a"
    }))
    .unwrap();
    assert_eq!(current.model_id.as_deref(), Some("model-a"));
    let legacy: RegistryAdminError = serde_json::from_value(json!({
        "code": "unknown_revision",
        "message": "provider revision is unknown",
        "provider_id": "alpha",
        "revision": 2
    }))
    .unwrap();
    assert_eq!(legacy.model_id, None);
}

#[test]
fn credential_requests_default_validate_and_reject_unknown_fields() {
    let request: RegistryAdminRequest = serde_json::from_value(json!({
        "operation": "list_credential_generations",
        "binding_id": "credential/private"
    }))
    .unwrap();
    assert_eq!(
        request,
        RegistryAdminRequest::ListCredentialGenerations {
            binding_id: "credential/private".into(),
            before_generation: None,
            limit: DEFAULT_REGISTRY_REVISION_PAGE_SIZE,
        }
    );
    request.validate().unwrap();
    assert_eq!(
        serde_json::from_value::<RegistryAdminRequest>(serde_json::to_value(&request).unwrap())
            .unwrap(),
        request
    );

    for invalid in [
        RegistryAdminRequest::InspectCredentialGeneration {
            binding_id: " ".into(),
            generation: 1,
        },
        RegistryAdminRequest::InspectCredentialGeneration {
            binding_id: "credential/private".into(),
            generation: 0,
        },
        RegistryAdminRequest::ListCredentialGenerations {
            binding_id: "credential/private".into(),
            before_generation: Some(0),
            limit: 50,
        },
    ] {
        assert_eq!(
            invalid.validate().unwrap_err().code,
            RegistryAdminErrorCode::InvalidRequest
        );
    }
    assert!(
        serde_json::from_value::<RegistryAdminRequest>(json!({
            "operation": "inspect_credential_generation",
            "binding_id": "credential/private",
            "generation": 1,
            "unexpected": true
        }))
        .is_err()
    );
}

#[test]
fn credential_response_only_exposes_binding_and_reference_digests() {
    let response = RegistryAdminResponse::Success {
        result: Box::new(RegistryAdminResult::CredentialGenerationInspected {
            generation: CredentialGenerationView {
                binding_id_sha256: "binding-digest".into(),
                generation: 2,
                reference_kind: CredentialReferenceKind::File,
                reference_configured: true,
                reference_digest_sha256: "reference-digest".into(),
                created_at_unix_secs: 9,
                active: true,
            },
        }),
    };
    let encoded = serde_json::to_value(response).unwrap();
    let generation = &encoded["result"]["generation"];
    assert_eq!(generation["binding_id_sha256"], "binding-digest");
    assert_eq!(generation["reference_kind"], "file");
    assert_eq!(generation["reference_digest_sha256"], "reference-digest");
    for field in ["binding_id", "reference", "path", "name", "secret_value"] {
        assert!(generation.get(field).is_none(), "response exposed {field}");
    }
    let text = serde_json::to_string(&encoded).unwrap();
    for secret in [
        "credential/private",
        "/run/private",
        "SECRET_ENV",
        "token-value",
    ] {
        assert!(!text.contains(secret), "response exposed {secret}");
    }
}

#[test]
fn credential_error_identity_is_hashed_and_new_fields_default_for_legacy_errors() {
    let current: RegistryAdminError = serde_json::from_value(json!({
        "code": "unknown_generation",
        "message": "credential generation is unknown",
        "binding_id_sha256": "binding-digest",
        "generation": 2
    }))
    .unwrap();
    assert_eq!(current.binding_id_sha256.as_deref(), Some("binding-digest"));
    assert_eq!(current.generation, Some(2));

    let legacy: RegistryAdminError = serde_json::from_value(json!({
        "code": "unknown_revision",
        "message": "provider revision is unknown",
        "provider_id": "alpha",
        "revision": 2
    }))
    .unwrap();
    assert_eq!(legacy.binding_id_sha256, None);
    assert_eq!(legacy.generation, None);
}
