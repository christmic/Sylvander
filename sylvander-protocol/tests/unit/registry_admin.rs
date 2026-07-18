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

#[test]
fn credential_lifecycle_requests_have_stable_wire_shapes() {
    let create: RegistryAdminRequest = serde_json::from_value(json!({
        "operation": "create_credential_binding",
        "binding_id": "credential/main",
        "reference": {"source": "environment", "name": "PROVIDER_TOKEN"}
    }))
    .unwrap();
    create.validate().unwrap();
    let encoded = serde_json::to_value(&create).unwrap();
    assert!(encoded.get("generation").is_none());
    assert_eq!(
        serde_json::from_value::<RegistryAdminRequest>(encoded).unwrap(),
        create
    );
    assert!(
        serde_json::from_value::<RegistryAdminRequest>(json!({
            "operation": "create_credential_binding",
            "binding_id": "credential/main",
            "generation": 1,
            "reference": {"source": "file", "path": "/run/key"}
        }))
        .is_err()
    );

    for request in [
        RegistryAdminRequest::StageCredentialGeneration {
            binding_id: "credential/main".into(),
            generation: 2,
            expected_active_generation: 1,
            reference: CredentialSecretReferenceDraft::File {
                path: "/run/key".into(),
            },
        },
        RegistryAdminRequest::ActivateCredentialGeneration {
            binding_id: "credential/main".into(),
            generation: 2,
            expected_active_generation: 1,
        },
        RegistryAdminRequest::RollbackCredentialGeneration {
            binding_id: "credential/main".into(),
            target_generation: 1,
            expected_active_generation: 2,
        },
    ] {
        request.validate().unwrap();
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<RegistryAdminRequest>(encoded).unwrap(),
            request
        );
    }
}

#[test]
fn credential_lifecycle_validation_rejects_blank_references_and_zero_generations() {
    for request in [
        RegistryAdminRequest::CreateCredentialBinding {
            binding_id: " ".into(),
            reference: CredentialSecretReferenceDraft::Environment {
                name: "TOKEN".into(),
            },
        },
        RegistryAdminRequest::CreateCredentialBinding {
            binding_id: "credential/main".into(),
            reference: CredentialSecretReferenceDraft::Environment { name: " ".into() },
        },
        RegistryAdminRequest::CreateCredentialBinding {
            binding_id: "credential/main".into(),
            reference: CredentialSecretReferenceDraft::File { path: " ".into() },
        },
        RegistryAdminRequest::StageCredentialGeneration {
            binding_id: "credential/main".into(),
            generation: 0,
            expected_active_generation: 1,
            reference: CredentialSecretReferenceDraft::Environment {
                name: "TOKEN".into(),
            },
        },
        RegistryAdminRequest::StageCredentialGeneration {
            binding_id: "credential/main".into(),
            generation: 2,
            expected_active_generation: 0,
            reference: CredentialSecretReferenceDraft::Environment {
                name: "TOKEN".into(),
            },
        },
        RegistryAdminRequest::ActivateCredentialGeneration {
            binding_id: "credential/main".into(),
            generation: 0,
            expected_active_generation: 1,
        },
        RegistryAdminRequest::RollbackCredentialGeneration {
            binding_id: "credential/main".into(),
            target_generation: 1,
            expected_active_generation: 0,
        },
    ] {
        assert_eq!(
            request.validate().unwrap_err().code,
            RegistryAdminErrorCode::InvalidRequest
        );
    }
}

#[test]
fn credential_reference_debug_and_wire_never_accept_secret_values() {
    let reference = CredentialSecretReferenceDraft::Environment {
        name: "PRIVATE_ENV_NAME".into(),
    };
    let debug = format!("{reference:?}");
    assert_eq!(debug, "CredentialSecretReferenceDraft([REDACTED])");
    assert!(!debug.contains("PRIVATE_ENV_NAME"));
    assert!(
        serde_json::from_value::<CredentialSecretReferenceDraft>(json!({
            "source": "environment",
            "name": "TOKEN",
            "secret_value": "private"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<CredentialSecretReferenceDraft>(json!({
            "source": "literal",
            "value": "private"
        }))
        .is_err()
    );
}

#[test]
fn credential_lifecycle_results_and_conflicts_are_typed_and_redacted() {
    let generation = CredentialGenerationView {
        binding_id_sha256: "binding-digest".into(),
        generation: 1,
        reference_kind: CredentialReferenceKind::Environment,
        reference_configured: true,
        reference_digest_sha256: "reference-digest".into(),
        created_at_unix_secs: 10,
        active: true,
    };
    for result in [
        RegistryAdminResult::CredentialBindingCreated {
            generation: generation.clone(),
        },
        RegistryAdminResult::CredentialGenerationStaged {
            generation: generation.clone(),
        },
        RegistryAdminResult::CredentialGenerationActivated {
            binding_id_sha256: "binding-digest".into(),
            active_generation: 1,
        },
        RegistryAdminResult::CredentialGenerationRolledBack {
            binding_id_sha256: "binding-digest".into(),
            active_generation: 1,
        },
    ] {
        let encoded = serde_json::to_string(&RegistryAdminResponse::Success {
            result: Box::new(result),
        })
        .unwrap();
        for private in [
            "credential/main",
            "PRIVATE_ENV_NAME",
            "/run/key",
            "secret_value",
        ] {
            assert!(!encoded.contains(private), "response exposed {private}");
        }
    }

    let error: RegistryAdminError = serde_json::from_value(json!({
        "code": "active_generation_conflict",
        "message": "active generation changed",
        "binding_id_sha256": "binding-digest",
        "details": {
            "kind": "active_generation_conflict",
            "expected_active_generation": 1,
            "actual_active_generation": 2
        }
    }))
    .unwrap();
    assert_eq!(
        error.details.as_deref(),
        Some(&RegistryAdminErrorDetails::ActiveGenerationConflict {
            expected_active_generation: 1,
            actual_active_generation: 2,
        })
    );
    let legacy: RegistryAdminError = serde_json::from_value(json!({
        "code": "unknown_generation",
        "message": "unknown",
        "binding_id_sha256": "binding-digest",
        "generation": 1
    }))
    .unwrap();
    assert_eq!(legacy.details, None);
}

fn provider_draft() -> ProviderDefinitionDraft {
    ProviderDefinitionDraft {
        kind: "anthropic_compatible".into(),
        base_url: "https://private.example.test".into(),
        credential_binding_id: "credential/private".into(),
    }
}

#[test]
fn provider_lifecycle_requests_validate_and_have_stable_wire_shapes() {
    let requests = [
        RegistryAdminRequest::CreateProvider {
            provider_id: "alpha".into(),
            definition: provider_draft(),
        },
        RegistryAdminRequest::StageProviderRevision {
            provider_id: "alpha".into(),
            revision: 2,
            expected_active_revision: 1,
            definition: provider_draft(),
        },
        RegistryAdminRequest::ActivateProviderRevision {
            provider_id: "alpha".into(),
            revision: 2,
            expected_active_revision: 1,
        },
        RegistryAdminRequest::RollbackProviderRevision {
            provider_id: "alpha".into(),
            target_revision: 1,
            expected_active_revision: 2,
        },
    ];
    for request in requests {
        request.validate().unwrap();
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<RegistryAdminRequest>(encoded).unwrap(),
            request
        );
        assert_eq!(
            request.minimum_ui_protocol_version(),
            crate::UI_PROTOCOL_VERSION
        );
    }

    assert!(
        serde_json::from_value::<RegistryAdminRequest>(json!({
            "operation": "create_provider",
            "provider_id": "alpha",
            "definition": {
                "kind": "anthropic_compatible",
                "base_url": "https://example.test",
                "credential_binding_id": "credential/alpha",
                "unexpected": true
            }
        }))
        .is_err()
    );
}

#[test]
fn provider_lifecycle_validation_and_draft_debug_are_content_free() {
    for request in [
        RegistryAdminRequest::CreateProvider {
            provider_id: " ".into(),
            definition: provider_draft(),
        },
        RegistryAdminRequest::CreateProvider {
            provider_id: "alpha".into(),
            definition: ProviderDefinitionDraft {
                kind: " ".into(),
                base_url: "https://example.test".into(),
                credential_binding_id: "credential/alpha".into(),
            },
        },
        RegistryAdminRequest::StageProviderRevision {
            provider_id: "alpha".into(),
            revision: 0,
            expected_active_revision: 1,
            definition: provider_draft(),
        },
        RegistryAdminRequest::ActivateProviderRevision {
            provider_id: "alpha".into(),
            revision: 2,
            expected_active_revision: 0,
        },
        RegistryAdminRequest::RollbackProviderRevision {
            provider_id: "alpha".into(),
            target_revision: 0,
            expected_active_revision: 2,
        },
    ] {
        assert_eq!(
            request.validate().unwrap_err().code,
            RegistryAdminErrorCode::InvalidRequest
        );
    }
    let debug = format!("{:?}", provider_draft());
    assert_eq!(debug, "ProviderDefinitionDraft([REDACTED])");
    for private in [
        "anthropic_compatible",
        "https://private.example.test",
        "credential/private",
    ] {
        assert!(!debug.contains(private));
    }
}

#[test]
fn provider_lifecycle_results_errors_and_schema_are_typed() {
    for result in [
        RegistryAdminResult::ProviderCreated { revision: view() },
        RegistryAdminResult::ProviderRevisionStaged { revision: view() },
        RegistryAdminResult::ProviderRevisionActivated { revision: view() },
        RegistryAdminResult::ProviderRevisionRolledBack { revision: view() },
    ] {
        let encoded = serde_json::to_value(RegistryAdminResponse::Success {
            result: Box::new(result),
        })
        .unwrap();
        assert!(encoded["result"]["revision"]["definition"]["base_url"].is_null());
        assert_eq!(
            encoded["result"]["revision"]["definition"]["base_url_sha256"],
            "base-digest"
        );
    }
    for (code, details) in [
        (
            RegistryAdminErrorCode::ActiveRevisionConflict,
            RegistryAdminErrorDetails::ActiveRevisionConflict {
                expected_active_revision: 1,
                actual_active_revision: 2,
            },
        ),
        (
            RegistryAdminErrorCode::NonSequentialRevision,
            RegistryAdminErrorDetails::NonSequentialRevision {
                expected_revision: 2,
                actual_revision: 4,
            },
        ),
        (
            RegistryAdminErrorCode::RevisionCollision,
            RegistryAdminErrorDetails::RevisionCollision { revision: 2 },
        ),
        (
            RegistryAdminErrorCode::InvalidRevisionRollback,
            RegistryAdminErrorDetails::InvalidRevisionRollback {
                target_revision: 3,
                actual_active_revision: 2,
            },
        ),
    ] {
        let encoded = serde_json::to_value(RegistryAdminError {
            code,
            message: "provider mutation failed".into(),
            provider_id: Some("alpha".into()),
            model_id: None,
            binding_id_sha256: None,
            revision: None,
            generation: None,
            details: Some(Box::new(details.clone())),
        })
        .unwrap();
        assert_eq!(
            serde_json::from_value::<RegistryAdminError>(encoded)
                .unwrap()
                .details
                .as_deref(),
            Some(&details)
        );
    }
    let rollback_code =
        serde_json::to_value(RegistryAdminErrorCode::InvalidRevisionRollback).unwrap();
    assert_eq!(rollback_code, "invalid_revision_rollback");
    let schema = serde_json::to_string(&schemars::schema_for!(RegistryAdminRequest)).unwrap();
    for operation in [
        "create_provider",
        "stage_provider_revision",
        "activate_provider_revision",
        "rollback_provider_revision",
    ] {
        assert!(schema.contains(operation));
    }
}

fn model_draft() -> ModelDefinitionDraft {
    ModelDefinitionDraft {
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: vec!["tool_use".into()],
        lifecycle: ModelLifecycleDraft::Active {},
        pricing: Some(ModelPricingDraft {
            input_usd_micros_per_million: 1,
            output_usd_micros_per_million: 2,
            cache_write_usd_micros_per_million: Some(3),
            cache_read_usd_micros_per_million: Some(4),
        }),
    }
}

#[test]
fn model_lifecycle_requests_validate_round_trip_and_require_current_protocol() {
    let requests = [
        RegistryAdminRequest::CreateModel {
            provider_id: "alpha".into(),
            model_id: "model-a".into(),
            definition: model_draft(),
        },
        RegistryAdminRequest::StageModelRevision {
            provider_id: "alpha".into(),
            model_id: "model-a".into(),
            revision: 2,
            expected_active_revision: 1,
            definition: model_draft(),
        },
        RegistryAdminRequest::ActivateModelRevision {
            provider_id: "alpha".into(),
            model_id: "model-a".into(),
            revision: 2,
            expected_active_revision: 1,
        },
        RegistryAdminRequest::RollbackModelRevision {
            provider_id: "alpha".into(),
            model_id: "model-a".into(),
            target_revision: 1,
            expected_active_revision: 2,
        },
    ];
    for request in requests {
        request.validate().unwrap();
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<RegistryAdminRequest>(wire).unwrap(),
            request
        );
        assert_eq!(
            request.minimum_ui_protocol_version(),
            crate::UI_PROTOCOL_VERSION
        );
    }
}

#[test]
fn model_draft_is_strict_validated_and_fully_redacted_in_debug() {
    for definition in [
        ModelDefinitionDraft {
            context_window: 0,
            ..model_draft()
        },
        ModelDefinitionDraft {
            max_output_tokens: 0,
            ..model_draft()
        },
        ModelDefinitionDraft {
            capabilities: vec![" ".into()],
            ..model_draft()
        },
        ModelDefinitionDraft {
            lifecycle: ModelLifecycleDraft::Deprecated {
                replacement: Some(" ".into()),
            },
            ..model_draft()
        },
        ModelDefinitionDraft {
            capabilities: vec!["tool_use".into(), "tool_use".into()],
            ..model_draft()
        },
    ] {
        let request = RegistryAdminRequest::CreateModel {
            provider_id: "alpha".into(),
            model_id: "model-a".into(),
            definition,
        };
        assert_eq!(
            request.validate().unwrap_err().code,
            RegistryAdminErrorCode::InvalidRequest
        );
    }
    for request in [
        RegistryAdminRequest::CreateModel {
            provider_id: " ".into(),
            model_id: "m".into(),
            definition: model_draft(),
        },
        RegistryAdminRequest::CreateModel {
            provider_id: "p".into(),
            model_id: " ".into(),
            definition: model_draft(),
        },
        RegistryAdminRequest::StageModelRevision {
            provider_id: "p".into(),
            model_id: "m".into(),
            revision: 0,
            expected_active_revision: 1,
            definition: model_draft(),
        },
        RegistryAdminRequest::ActivateModelRevision {
            provider_id: "p".into(),
            model_id: "m".into(),
            revision: 2,
            expected_active_revision: 0,
        },
        RegistryAdminRequest::RollbackModelRevision {
            provider_id: "p".into(),
            model_id: "m".into(),
            target_revision: 0,
            expected_active_revision: 2,
        },
    ] {
        assert_eq!(
            request.validate().unwrap_err().code,
            RegistryAdminErrorCode::InvalidRequest
        );
    }
    for debug in [
        format!("{:?}", model_draft()),
        format!(
            "{:?}",
            ModelLifecycleDraft::Deprecated {
                replacement: Some("private-model".into())
            }
        ),
        format!("{:?}", model_draft().pricing.unwrap()),
    ] {
        assert!(debug.contains("[REDACTED]"));
        for private in ["tool_use", "private-model", "100000", "4096"] {
            assert!(!debug.contains(private));
        }
    }
    for invalid in [
        json!({"context_window":1,"max_output_tokens":1,"capabilities":[],"lifecycle":{"status":"active"},"unexpected":true}),
        json!({"context_window":1,"max_output_tokens":1,"capabilities":[],"lifecycle":{"status":"active","replacement":"x"}}),
        json!({"context_window":1,"max_output_tokens":1,"capabilities":[],"lifecycle":{"status":"active"},"pricing":{"input_usd_micros_per_million":1,"output_usd_micros_per_million":2,"unexpected":3}}),
    ] {
        assert!(serde_json::from_value::<ModelDefinitionDraft>(invalid).is_err());
    }
    let deprecated: ModelLifecycleDraft =
        serde_json::from_value(json!({"status":"deprecated"})).unwrap();
    assert_eq!(
        deprecated,
        ModelLifecycleDraft::Deprecated { replacement: None }
    );
    assert_eq!(
        serde_json::to_value(deprecated).unwrap(),
        json!({"status":"deprecated"})
    );
}

#[test]
fn model_lifecycle_results_error_and_schema_are_typed_and_redacted() {
    for result in [
        RegistryAdminResult::ModelCreated {
            revision: model_view(),
        },
        RegistryAdminResult::ModelRevisionStaged {
            revision: model_view(),
        },
        RegistryAdminResult::ModelRevisionActivated {
            revision: model_view(),
        },
        RegistryAdminResult::ModelRevisionRolledBack {
            revision: model_view(),
        },
    ] {
        let wire = serde_json::to_string(&RegistryAdminResponse::Success {
            result: Box::new(result),
        })
        .unwrap();
        assert!(wire.contains("pricing_sha256"));
        assert!(!wire.contains("input_usd_micros_per_million"));
    }
    assert_eq!(
        serde_json::to_value(RegistryAdminErrorCode::ModelAlreadyExists).unwrap(),
        "model_already_exists"
    );
    let schema = serde_json::to_string(&schemars::schema_for!(RegistryAdminRequest)).unwrap();
    for operation in [
        "create_model",
        "stage_model_revision",
        "activate_model_revision",
        "rollback_model_revision",
    ] {
        assert!(schema.contains(operation));
    }
}

#[test]
fn every_registry_request_has_an_explicit_minimum_protocol_version() {
    let reads = [
        json!({"operation":"inspect_provider_revision","provider_id":"p","revision":1}),
        json!({"operation":"list_provider_revisions","provider_id":"p"}),
        json!({"operation":"inspect_model_revision","provider_id":"p","model_id":"m","revision":1}),
        json!({"operation":"list_model_revisions","provider_id":"p","model_id":"m"}),
        json!({"operation":"inspect_credential_generation","binding_id":"b","generation":1}),
        json!({"operation":"list_credential_generations","binding_id":"b"}),
    ];
    let mutations = [
        json!({"operation":"create_provider","provider_id":"p","definition":{"kind":"anthropic_compatible","base_url":"https://example.test","credential_binding_id":"b"}}),
        json!({"operation":"stage_provider_revision","provider_id":"p","revision":2,"expected_active_revision":1,"definition":{"kind":"anthropic_compatible","base_url":"https://example.test","credential_binding_id":"b"}}),
        json!({"operation":"activate_provider_revision","provider_id":"p","revision":2,"expected_active_revision":1}),
        json!({"operation":"rollback_provider_revision","provider_id":"p","target_revision":1,"expected_active_revision":2}),
        json!({"operation":"create_model","provider_id":"p","model_id":"m","definition":{"context_window":1,"max_output_tokens":1,"capabilities":[],"lifecycle":{"status":"active"}}}),
        json!({"operation":"stage_model_revision","provider_id":"p","model_id":"m","revision":2,"expected_active_revision":1,"definition":{"context_window":1,"max_output_tokens":1,"capabilities":[],"lifecycle":{"status":"active"}}}),
        json!({"operation":"activate_model_revision","provider_id":"p","model_id":"m","revision":2,"expected_active_revision":1}),
        json!({"operation":"rollback_model_revision","provider_id":"p","model_id":"m","target_revision":1,"expected_active_revision":2}),
        json!({"operation":"create_credential_binding","binding_id":"b","reference":{"source":"environment","name":"K"}}),
        json!({"operation":"stage_credential_generation","binding_id":"b","generation":2,"expected_active_generation":1,"reference":{"source":"file","path":"/k"}}),
        json!({"operation":"activate_credential_generation","binding_id":"b","generation":2,"expected_active_generation":1}),
        json!({"operation":"rollback_credential_generation","binding_id":"b","target_generation":1,"expected_active_generation":2}),
    ];
    for wire in reads {
        let request: RegistryAdminRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(
            request.minimum_ui_protocol_version(),
            crate::UI_PROTOCOL_VERSION
        );
    }
    for wire in mutations {
        let request: RegistryAdminRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(
            request.minimum_ui_protocol_version(),
            crate::UI_PROTOCOL_VERSION
        );
    }
}
