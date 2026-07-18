use std::collections::BTreeSet;

use sylvander_protocol::{
    AuthenticationMethod, ModelLifecycle, ModelPricing, RegistryAdminErrorCode,
};

use super::*;
use crate::config::{SecretRef, SystemSecretResolver};
use crate::credential_audit::{
    CredentialAuditOperation, CredentialAuditResult, CredentialAuditSubject,
    CredentialOperationAuditLedger,
};
use crate::registry_domain::CredentialBindingRevision;
use crate::request_scoped_provider::ProviderFactoryError;

const RAW_URL: &str = "https://user:RAW_URL_SECRET@example.invalid/path?token=leak";
const RAW_BINDING: &str = "RAW_BINDING_SECRET";

fn provider(revision: u64, base_url: &str) -> ProviderDefinition {
    ProviderDefinition {
        id: "alpha".into(),
        revision,
        kind: "anthropic_compatible".into(),
        base_url: base_url.into(),
        credential_binding_id: RAW_BINDING.into(),
    }
}

fn provider_draft(base_url: &str) -> ProviderDefinitionDraft {
    ProviderDefinitionDraft {
        kind: "anthropic_compatible".into(),
        base_url: base_url.into(),
        credential_binding_id: RAW_BINDING.into(),
    }
}

fn model(revision: u64) -> ModelDefinition {
    ModelDefinition {
        provider_id: "alpha".into(),
        model_id: "shared".into(),
        revision,
        context_window: 100_000 + u32::try_from(revision).unwrap(),
        max_output_tokens: 4096,
        capabilities: BTreeSet::from(["tool_use".into()]),
        lifecycle: ModelLifecycle::Active,
        pricing: Some(ModelPricing {
            input_usd_micros_per_million: revision * 100,
            output_usd_micros_per_million: revision * 200,
            cache_write_usd_micros_per_million: None,
            cache_read_usd_micros_per_million: None,
        }),
    }
}

fn model_draft(context_window: u32, input_price: u64) -> ModelDefinitionDraft {
    ModelDefinitionDraft {
        context_window,
        max_output_tokens: 4096,
        capabilities: vec!["tool_use".into()],
        lifecycle: ModelLifecycleDraft::Active {},
        pricing: Some(ModelPricingDraft {
            input_usd_micros_per_million: input_price,
            output_usd_micros_per_million: input_price + 1,
            cache_write_usd_micros_per_million: None,
            cache_read_usd_micros_per_million: None,
        }),
    }
}

async fn registry() -> AgentRegistry {
    let registry = AgentRegistry::open(":memory:").await.unwrap();
    registry
        .seed_credential(CredentialBindingRevision {
            binding_id: RAW_BINDING.into(),
            generation: 1,
            reference: SecretRef::Env {
                name: "UNRESOLVED_TEST_REFERENCE".into(),
            },
        })
        .await
        .unwrap();
    registry.seed_provider(provider(1, RAW_URL)).await.unwrap();
    registry
}

#[test]
fn compatibility_failures_map_to_fixed_redacted_protocol_errors() {
    let model = map_model_error(
        ModelRegistryError::IncompatibleProvider(ProviderFactoryError::UnsupportedKind),
        "provider-secret".into(),
        "model-secret".into(),
        Some(2),
    );
    let provider = map_provider_error(
        ProviderRegistryError::IncompatibleModel(ProviderFactoryError::InvalidModelDefinition),
        "provider-secret".into(),
        Some(2),
    );
    assert_eq!(model.code, RegistryAdminErrorCode::InvalidRequest);
    assert_eq!(provider.code, RegistryAdminErrorCode::InvalidRequest);
    for response in [failure(model), failure(provider)] {
        let wire = serde_json::to_string(&response).unwrap();
        assert!(!wire.contains("UnsupportedKind"));
        assert!(!wire.contains("InvalidModelDefinition"));
    }
}

fn admin() -> AuthenticatedPrincipal {
    let mut principal = AuthenticatedPrincipal::user("operator", AuthenticationMethod::Internal);
    principal.roles.push("admin".into());
    principal
}

#[tokio::test]
async fn exact_inspection_stays_pinned_and_redacted_after_head_moves() {
    let registry = registry().await;
    registry
        .stage_provider(1, provider(2, "https://new.invalid"))
        .await
        .unwrap();
    registry.activate_provider("alpha", 2, 1).await.unwrap();
    let response = RegistryAdminService::new(&registry)
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::InspectProviderRevision {
                provider_id: "alpha".into(),
                revision: 1,
            },
        )
        .await;
    let encoded = serde_json::to_string(&response).unwrap();
    let debug = format!("{response:?}");
    for marker in [RAW_URL, "RAW_URL_SECRET", RAW_BINDING] {
        assert!(!encoded.contains(marker));
        assert!(!debug.contains(marker));
    }
    let RegistryAdminResponse::Success { result } = response else {
        panic!("expected success");
    };
    let RegistryAdminResult::ProviderRevisionInspected { revision } = *result else {
        panic!("expected inspection");
    };
    assert_eq!(revision.definition.revision, 1);
    assert!(!revision.active);
    assert_eq!(revision.definition.base_url_sha256, sha256(RAW_URL));
    assert_eq!(
        revision.definition.credential_binding_id_sha256,
        sha256(RAW_BINDING)
    );
}

#[tokio::test]
async fn list_is_descending_paginated_and_reports_active_revision() {
    let registry = registry().await;
    for revision in 2..=3 {
        registry
            .stage_provider(
                1,
                provider(revision, &format!("https://v{revision}.invalid")),
            )
            .await
            .unwrap();
    }
    registry.activate_provider("alpha", 3, 1).await.unwrap();
    let service = RegistryAdminService::new(&registry);
    let first = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::ListProviderRevisions {
                provider_id: "alpha".into(),
                before_revision: None,
                limit: 2,
            },
        )
        .await;
    let RegistryAdminResponse::Success { result } = first else {
        panic!("expected first page");
    };
    let RegistryAdminResult::ProviderRevisionsListed {
        active_revision,
        revisions,
        next_before_revision,
        ..
    } = *result
    else {
        panic!("expected list");
    };
    assert_eq!(active_revision, 3);
    assert_eq!(
        revisions
            .iter()
            .map(|item| item.definition.revision)
            .collect::<Vec<_>>(),
        [3, 2]
    );
    assert_eq!(next_before_revision, Some(2));

    let second = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::ListProviderRevisions {
                provider_id: "alpha".into(),
                before_revision: next_before_revision,
                limit: 2,
            },
        )
        .await;
    let RegistryAdminResponse::Success { result } = second else {
        panic!("expected second page");
    };
    let RegistryAdminResult::ProviderRevisionsListed {
        revisions,
        next_before_revision,
        ..
    } = *result
    else {
        panic!("expected list");
    };
    assert_eq!(revisions[0].definition.revision, 1);
    assert_eq!(next_before_revision, None);
}

#[tokio::test]
async fn provider_lifecycle_is_redacted_and_reports_typed_cas_failures() {
    let registry = registry().await;
    let service = RegistryAdminService::new(&registry);
    let principal = admin();
    let dispatch = |request| service.dispatch(Some(&principal), request);

    let created = dispatch(RegistryAdminRequest::CreateProvider {
        provider_id: "beta".into(),
        definition: provider_draft("https://CREATE_SECRET.invalid"),
    })
    .await;
    assert!(matches!(
        created,
        RegistryAdminResponse::Success { ref result }
            if matches!(result.as_ref(), RegistryAdminResult::ProviderCreated { revision }
                if revision.active && revision.definition.revision == 1)
    ));

    let duplicate = dispatch(RegistryAdminRequest::CreateProvider {
        provider_id: "beta".into(),
        definition: provider_draft("https://DIFFERENT_SECRET.invalid"),
    })
    .await;
    assert!(matches!(
        duplicate,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::ProviderAlreadyExists
    ));

    let nonsequential = dispatch(RegistryAdminRequest::StageProviderRevision {
        provider_id: "beta".into(),
        revision: 3,
        expected_active_revision: 1,
        definition: provider_draft("https://THREE_SECRET.invalid"),
    })
    .await;
    assert!(matches!(
        nonsequential,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::NonSequentialRevision
                && matches!(error.details.as_deref(), Some(
                    RegistryAdminErrorDetails::NonSequentialRevision {
                        expected_revision: 2,
                        actual_revision: 3,
                    }
                ))
    ));

    let staged = dispatch(RegistryAdminRequest::StageProviderRevision {
        provider_id: "beta".into(),
        revision: 2,
        expected_active_revision: 1,
        definition: provider_draft("https://TWO_SECRET.invalid"),
    })
    .await;
    assert!(matches!(
        staged,
        RegistryAdminResponse::Success { ref result }
            if matches!(result.as_ref(), RegistryAdminResult::ProviderRevisionStaged { revision }
                if !revision.active && revision.definition.revision == 2)
    ));
    let activated = dispatch(RegistryAdminRequest::ActivateProviderRevision {
        provider_id: "beta".into(),
        revision: 2,
        expected_active_revision: 1,
    })
    .await;
    assert!(matches!(
        activated,
        RegistryAdminResponse::Success { ref result }
            if matches!(result.as_ref(), RegistryAdminResult::ProviderRevisionActivated { revision }
                if revision.active && revision.definition.revision == 2)
    ));

    for response in [created, staged, activated] {
        let wire = serde_json::to_string(&response).unwrap();
        for secret in [RAW_BINDING, "CREATE_SECRET", "TWO_SECRET"] {
            assert!(!wire.contains(secret));
        }
    }

    let conflict = dispatch(RegistryAdminRequest::RollbackProviderRevision {
        provider_id: "beta".into(),
        target_revision: 1,
        expected_active_revision: 1,
    })
    .await;
    assert!(matches!(
        conflict,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::ActiveRevisionConflict
                && matches!(error.details.as_deref(), Some(
                    RegistryAdminErrorDetails::ActiveRevisionConflict {
                        expected_active_revision: 1,
                        actual_active_revision: 2,
                    }
                ))
    ));
}

#[tokio::test]
async fn provider_lifecycle_preflights_adapter_without_mutating_heads() {
    let registry = registry().await;
    let service = RegistryAdminService::new(&registry);
    let principal = admin();
    let invalid_url = "NOT_A_PROVIDER_URL_SECRET";
    let rejected = service
        .dispatch(
            Some(&principal),
            RegistryAdminRequest::CreateProvider {
                provider_id: "invalid".into(),
                definition: provider_draft(invalid_url),
            },
        )
        .await;
    assert!(matches!(
        rejected,
        RegistryAdminResponse::Error { ref error }
            if error.code == RegistryAdminErrorCode::InvalidRequest
    ));
    assert!(
        !serde_json::to_string(&rejected)
            .unwrap()
            .contains(invalid_url)
    );
    assert!(
        registry
            .load_active_provider("invalid")
            .await
            .unwrap()
            .is_none()
    );

    let mut first = provider(1, "https://valid.invalid");
    first.id = "legacy".into();
    registry.create_provider(first).await.unwrap();
    let mut unsupported = provider(2, "https://valid.invalid");
    unsupported.id = "legacy".into();
    unsupported.kind = "UNSUPPORTED_KIND_SECRET".into();
    registry.stage_provider(1, unsupported).await.unwrap();
    let rejected = service
        .dispatch(
            Some(&principal),
            RegistryAdminRequest::ActivateProviderRevision {
                provider_id: "legacy".into(),
                revision: 2,
                expected_active_revision: 1,
            },
        )
        .await;
    assert!(matches!(
        rejected,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::InvalidRequest
    ));
    assert_eq!(
        registry
            .load_active_provider("legacy")
            .await
            .unwrap()
            .unwrap()
            .definition
            .revision,
        1
    );
}

#[tokio::test]
async fn unknown_and_unauthorized_fail_with_fixed_typed_errors() {
    let registry = registry().await;
    let service = RegistryAdminService::new(&registry);
    let request = RegistryAdminRequest::InspectProviderRevision {
        provider_id: "missing".into(),
        revision: 7,
    };
    let unauthorized = service.dispatch(None, request.clone()).await;
    let unknown = service.dispatch(Some(&admin()), request).await;
    assert!(matches!(
        unauthorized,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::Unauthorized
                && error.message == "registry administration requires an administrator"
    ));
    assert!(matches!(
        unknown,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::UnknownRevision
                && error.message == "provider revision is unknown"
    ));
}

#[tokio::test]
async fn model_reads_are_pinned_paginated_and_pricing_redacted() {
    let registry = registry().await;
    registry.seed_model(model(1)).await.unwrap();
    registry.stage_model(1, model(2)).await.unwrap();
    registry.stage_model(1, model(3)).await.unwrap();
    registry
        .activate_model(("alpha", "shared"), 3, 1)
        .await
        .unwrap();
    let service = RegistryAdminService::new(&registry);

    let inspected = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::InspectModelRevision {
                provider_id: "alpha".into(),
                model_id: "shared".into(),
                revision: 1,
            },
        )
        .await;
    let encoded = serde_json::to_string(&inspected).unwrap();
    for raw_price in [
        "input_usd_micros_per_million",
        "output_usd_micros_per_million",
    ] {
        assert!(!encoded.contains(raw_price));
    }
    let RegistryAdminResponse::Success { result } = inspected else {
        panic!("expected model inspection");
    };
    let RegistryAdminResult::ModelRevisionInspected { revision } = *result else {
        panic!("expected model revision");
    };
    assert_eq!(revision.definition.revision, 1);
    assert!(!revision.active);
    assert!(revision.definition.pricing_sha256.is_some());

    let first = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::ListModelRevisions {
                provider_id: "alpha".into(),
                model_id: "shared".into(),
                before_revision: None,
                limit: 2,
            },
        )
        .await;
    let RegistryAdminResponse::Success { result } = first else {
        panic!("expected model list");
    };
    let RegistryAdminResult::ModelRevisionsListed {
        active_revision,
        revisions,
        next_before_revision,
        ..
    } = *result
    else {
        panic!("expected model revisions");
    };
    assert_eq!(active_revision, 3);
    assert_eq!(
        revisions
            .iter()
            .map(|item| item.definition.revision)
            .collect::<Vec<_>>(),
        [3, 2]
    );
    assert_eq!(next_before_revision, Some(2));

    let unknown = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::ListModelRevisions {
                provider_id: "alpha".into(),
                model_id: "missing".into(),
                before_revision: None,
                limit: 1,
            },
        )
        .await;
    assert!(matches!(
        unknown,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::UnknownModel
                && error.provider_id.as_deref() == Some("alpha")
                && error.model_id.as_deref() == Some("missing")
    ));
}

#[tokio::test]
async fn model_capability_ingress_is_canonical_and_fails_before_mutation() {
    let registry = registry().await;
    let service = RegistryAdminService::new(&registry);
    let principal = admin();

    let mut reasoning = model_draft(100_000, 1);
    reasoning.capabilities = vec!["reasoning".into()];
    let created = service
        .dispatch(
            Some(&principal),
            RegistryAdminRequest::CreateModel {
                provider_id: "alpha".into(),
                model_id: "reasoning-model".into(),
                definition: reasoning,
            },
        )
        .await;
    assert!(matches!(
        created,
        RegistryAdminResponse::Success { result }
            if matches!(result.as_ref(), RegistryAdminResult::ModelCreated { .. })
    ));
    assert_eq!(
        registry
            .load_active_model(("alpha", "reasoning-model"))
            .await
            .unwrap()
            .unwrap()
            .definition
            .capabilities,
        BTreeSet::from(["extended_thinking".into()])
    );

    for (index, capabilities) in [
        vec!["telepathy"],
        vec![" tool_use"],
        vec!["TOOL_USE"],
        vec!["tool_use", "tool_use"],
        vec!["reasoning", "extended_thinking"],
    ]
    .into_iter()
    .enumerate()
    {
        let model_id = format!("invalid-capability-{index}");
        let mut draft = model_draft(100_000, 1);
        draft.capabilities = capabilities.iter().map(|value| (*value).into()).collect();
        let rejected = service
            .dispatch(
                Some(&principal),
                RegistryAdminRequest::CreateModel {
                    provider_id: "alpha".into(),
                    model_id: model_id.clone(),
                    definition: draft,
                },
            )
            .await;
        assert!(matches!(
            rejected,
            RegistryAdminResponse::Error { ref error }
                if error.code == RegistryAdminErrorCode::InvalidRequest
        ));
        let encoded = serde_json::to_string(&rejected).unwrap();
        assert!(
            capabilities
                .iter()
                .all(|capability| !encoded.contains(capability))
        );
        assert!(
            registry
                .load_active_model(("alpha", &model_id))
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn model_lifecycle_is_redacted_and_maps_revision_conflicts() {
    let registry = registry().await;
    let service = RegistryAdminService::new(&registry);
    let principal = admin();
    let dispatch = |request| service.dispatch(Some(&principal), request);

    let created = dispatch(RegistryAdminRequest::CreateModel {
        provider_id: "alpha".into(),
        model_id: "lifecycle".into(),
        definition: model_draft(100_000, 900_001),
    })
    .await;
    assert!(matches!(
        created,
        RegistryAdminResponse::Success { ref result }
            if matches!(result.as_ref(), RegistryAdminResult::ModelCreated { revision }
                if revision.active && revision.definition.revision == 1)
    ));
    let created_wire = serde_json::to_string(&created).unwrap();
    assert!(!created_wire.contains("900001"));
    assert!(!created_wire.contains("input_usd_micros_per_million"));

    let duplicate = dispatch(RegistryAdminRequest::CreateModel {
        provider_id: "alpha".into(),
        model_id: "lifecycle".into(),
        definition: model_draft(200_000, 1),
    })
    .await;
    assert!(matches!(
        duplicate,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::ModelAlreadyExists
    ));

    let staged = dispatch(RegistryAdminRequest::StageModelRevision {
        provider_id: "alpha".into(),
        model_id: "lifecycle".into(),
        revision: 2,
        expected_active_revision: 1,
        definition: model_draft(200_000, 900_002),
    })
    .await;
    assert!(matches!(
        staged,
        RegistryAdminResponse::Success { ref result }
            if matches!(result.as_ref(), RegistryAdminResult::ModelRevisionStaged { revision }
                if !revision.active && revision.definition.revision == 2)
    ));
    let activated = dispatch(RegistryAdminRequest::ActivateModelRevision {
        provider_id: "alpha".into(),
        model_id: "lifecycle".into(),
        revision: 2,
        expected_active_revision: 1,
    })
    .await;
    assert!(matches!(
        activated,
        RegistryAdminResponse::Success { ref result }
            if matches!(result.as_ref(), RegistryAdminResult::ModelRevisionActivated { revision }
                if revision.active && revision.definition.revision == 2)
    ));

    let _ = dispatch(RegistryAdminRequest::StageModelRevision {
        provider_id: "alpha".into(),
        model_id: "lifecycle".into(),
        revision: 3,
        expected_active_revision: 2,
        definition: model_draft(300_000, 900_003),
    })
    .await;
    let collision = dispatch(RegistryAdminRequest::StageModelRevision {
        provider_id: "alpha".into(),
        model_id: "lifecycle".into(),
        revision: 3,
        expected_active_revision: 2,
        definition: model_draft(333_000, 3),
    })
    .await;
    assert!(matches!(
        collision,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::RevisionCollision
                && matches!(error.details.as_deref(), Some(
                    RegistryAdminErrorDetails::RevisionCollision { revision: 3 }
                ))
    ));
    let nonsequential = dispatch(RegistryAdminRequest::StageModelRevision {
        provider_id: "alpha".into(),
        model_id: "lifecycle".into(),
        revision: 5,
        expected_active_revision: 2,
        definition: model_draft(500_000, 5),
    })
    .await;
    assert!(matches!(
        nonsequential,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::NonSequentialRevision
                && matches!(error.details.as_deref(), Some(
                    RegistryAdminErrorDetails::NonSequentialRevision {
                        expected_revision: 4,
                        actual_revision: 5,
                    }
                ))
    ));
    let invalid_rollback = dispatch(RegistryAdminRequest::RollbackModelRevision {
        provider_id: "alpha".into(),
        model_id: "lifecycle".into(),
        target_revision: 3,
        expected_active_revision: 2,
    })
    .await;
    assert!(matches!(
        invalid_rollback,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::InvalidRevisionRollback
                && matches!(error.details.as_deref(), Some(
                    RegistryAdminErrorDetails::InvalidRevisionRollback {
                        target_revision: 3,
                        actual_active_revision: 2,
                    }
                ))
    ));

    let conflict = dispatch(RegistryAdminRequest::RollbackModelRevision {
        provider_id: "alpha".into(),
        model_id: "lifecycle".into(),
        target_revision: 1,
        expected_active_revision: 1,
    })
    .await;
    assert!(matches!(
        conflict,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::ActiveRevisionConflict
                && matches!(error.details.as_deref(), Some(
                    RegistryAdminErrorDetails::ActiveRevisionConflict {
                        expected_active_revision: 1,
                        actual_active_revision: 2,
                    }
                ))
    ));

    let rolled_back = dispatch(RegistryAdminRequest::RollbackModelRevision {
        provider_id: "alpha".into(),
        model_id: "lifecycle".into(),
        target_revision: 1,
        expected_active_revision: 2,
    })
    .await;
    assert!(matches!(
        rolled_back,
        RegistryAdminResponse::Success { result }
            if matches!(result.as_ref(), RegistryAdminResult::ModelRevisionRolledBack { revision }
                if revision.active && revision.definition.revision == 1)
    ));
}

#[tokio::test]
async fn credential_reads_are_bounded_hashed_and_never_resolve_secrets() {
    let registry = registry().await;
    for generation in 2..=3 {
        registry
            .stage_credential(
                1,
                CredentialBindingRevision {
                    binding_id: RAW_BINDING.into(),
                    generation,
                    reference: SecretRef::File {
                        path: format!("/private/credential-{generation}").into(),
                    },
                },
            )
            .await
            .unwrap();
    }
    registry
        .activate_credential(RAW_BINDING, 3, 1)
        .await
        .unwrap();
    let service = RegistryAdminService::new(&registry);

    let inspected = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::InspectCredentialGeneration {
                binding_id: RAW_BINDING.into(),
                generation: 1,
            },
        )
        .await;
    let encoded = serde_json::to_string(&inspected).unwrap();
    let debug = format!("{inspected:?}");
    for marker in [
        RAW_BINDING,
        "UNRESOLVED_TEST_REFERENCE",
        "/private/credential-2",
    ] {
        assert!(!encoded.contains(marker));
        assert!(!debug.contains(marker));
    }
    let RegistryAdminResponse::Success { result } = inspected else {
        panic!("expected credential inspection");
    };
    let RegistryAdminResult::CredentialGenerationInspected { generation } = *result else {
        panic!("expected credential generation");
    };
    assert_eq!(generation.binding_id_sha256, sha256(RAW_BINDING));
    assert_eq!(generation.generation, 1);
    assert!(!generation.active);
    assert_eq!(
        generation.reference_kind,
        CredentialReferenceKind::Environment
    );

    let listed = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::ListCredentialGenerations {
                binding_id: RAW_BINDING.into(),
                before_generation: None,
                limit: 2,
            },
        )
        .await;
    let RegistryAdminResponse::Success { result } = listed else {
        panic!("expected credential list");
    };
    let RegistryAdminResult::CredentialGenerationsListed {
        binding_id_sha256,
        active_generation,
        generations,
        next_before_generation,
    } = *result
    else {
        panic!("expected credential generations");
    };
    assert_eq!(binding_id_sha256, sha256(RAW_BINDING));
    assert_eq!(active_generation, 3);
    assert_eq!(
        generations
            .iter()
            .map(|item| item.generation)
            .collect::<Vec<_>>(),
        [3, 2]
    );
    assert_eq!(next_before_generation, Some(2));

    let unknown = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::InspectCredentialGeneration {
                binding_id: RAW_BINDING.into(),
                generation: 99,
            },
        )
        .await;
    assert!(matches!(
        unknown,
        RegistryAdminResponse::Error { error }
            if error.code == RegistryAdminErrorCode::UnknownGeneration
                && error.binding_id_sha256.as_deref() == Some(sha256(RAW_BINDING).as_str())
                && error.generation == Some(99)
    ));
}

#[tokio::test]
async fn credential_mutations_preflight_cas_and_redact_every_response() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.secret");
    let second = directory.path().join("second.secret");
    let missing = directory.path().join("missing.secret");
    std::fs::write(&first, "first-value").unwrap();
    std::fs::write(&second, "second-value").unwrap();
    let binding_id = "credential/private-admin";
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let audit = CredentialOperationAuditLedger::open(directory.path().join("credential-audit.db"))
        .await
        .unwrap();
    let service = CredentialRegistryMutationService::new(&registry, &SystemSecretResolver, &audit);

    let created = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::CreateCredentialBinding {
                binding_id: binding_id.into(),
                reference: CredentialSecretReferenceDraft::File {
                    path: first.display().to_string(),
                },
            },
        )
        .await;
    assert!(matches!(
        created,
        RegistryAdminResponse::Success { ref result }
            if matches!(result.as_ref(), RegistryAdminResult::CredentialBindingCreated {
                generation
            } if generation.generation == 1 && generation.active)
    ));

    let staged = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::StageCredentialGeneration {
                binding_id: binding_id.into(),
                generation: 2,
                expected_active_generation: 1,
                reference: CredentialSecretReferenceDraft::File {
                    path: second.display().to_string(),
                },
            },
        )
        .await;
    assert!(matches!(
        staged,
        RegistryAdminResponse::Success { ref result }
            if matches!(result.as_ref(), RegistryAdminResult::CredentialGenerationStaged {
                generation
            } if generation.generation == 2 && !generation.active)
    ));

    let unavailable = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::StageCredentialGeneration {
                binding_id: binding_id.into(),
                generation: 3,
                expected_active_generation: 1,
                reference: CredentialSecretReferenceDraft::File {
                    path: missing.display().to_string(),
                },
            },
        )
        .await;
    assert!(matches!(unavailable, RegistryAdminResponse::Success { .. }));
    let unavailable = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::ActivateCredentialGeneration {
                binding_id: binding_id.into(),
                generation: 3,
                expected_active_generation: 1,
            },
        )
        .await;
    assert!(matches!(
        unavailable,
        RegistryAdminResponse::Error { ref error }
            if error.code == RegistryAdminErrorCode::CredentialUnavailable
                && error.generation == Some(3)
    ));

    let activated = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::ActivateCredentialGeneration {
                binding_id: binding_id.into(),
                generation: 2,
                expected_active_generation: 1,
            },
        )
        .await;
    assert!(matches!(activated, RegistryAdminResponse::Success { .. }));
    let conflict = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::RollbackCredentialGeneration {
                binding_id: binding_id.into(),
                target_generation: 1,
                expected_active_generation: 1,
            },
        )
        .await;
    assert!(matches!(
        conflict,
        RegistryAdminResponse::Error { ref error }
            if error.code == RegistryAdminErrorCode::ActiveGenerationConflict
                && matches!(error.details.as_deref(), Some(
                    RegistryAdminErrorDetails::ActiveGenerationConflict {
                        expected_active_generation: 1,
                        actual_active_generation: 2,
                    }
                ))
    ));
    let revoked = service
        .dispatch(
            Some(&admin()),
            RegistryAdminRequest::RollbackCredentialGeneration {
                binding_id: binding_id.into(),
                target_generation: 1,
                expected_active_generation: 2,
            },
        )
        .await;
    assert!(matches!(revoked, RegistryAdminResponse::Success { .. }));

    let subject = CredentialAuditSubject::provider_binding(binding_id).unwrap();
    let events = audit.list(&subject, 20).await.unwrap();
    assert!(events.iter().any(|event| {
        event.operation == CredentialAuditOperation::Create
            && event.result == CredentialAuditResult::Succeeded
    }));
    assert!(events.iter().any(|event| {
        event.operation == CredentialAuditOperation::Revoke
            && event.result == CredentialAuditResult::Succeeded
    }));
    assert!(events.iter().any(|event| {
        event.operation == CredentialAuditOperation::Failure
            && event.result == CredentialAuditResult::Conflict
    }));

    for response in [
        &created,
        &staged,
        &unavailable,
        &activated,
        &conflict,
        &revoked,
    ] {
        let rendered = format!("{response:?} {}", serde_json::to_string(response).unwrap());
        for secret in [
            binding_id,
            first.to_string_lossy().as_ref(),
            second.to_string_lossy().as_ref(),
            missing.to_string_lossy().as_ref(),
            "first-value",
            "second-value",
        ] {
            assert!(!rendered.contains(secret), "response leaked {secret}");
        }
    }
}
