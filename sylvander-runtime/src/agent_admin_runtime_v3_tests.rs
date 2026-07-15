use sylvander_protocol::{
    AgentAdminErrorCode, AgentAdminRequest, AgentAdminResponse, AgentAdminResult,
    AuthenticatedPrincipal, AuthenticationMethod, BoundaryContext, ModelCapability, ModelSelection,
    RegistryAdminResponse, SessionConfigOverrides, SessionConfigUpdateRequest,
    SessionCreateRequest,
};
use tempfile::TempDir;

use super::*;
use crate::agent_registry::AgentRegistryError;
use crate::registry_bootstrap::RegistryBootstrapPlan;

fn model(provider_id: &str) -> ModelSelection {
    ModelSelection {
        provider_id: provider_id.into(),
        model_id: "shared".into(),
    }
}

struct AdminV3Fixture {
    _directory: TempDir,
    runtime: Runtime,
    config: ServerConfig,
    administrator: BoundaryContext,
}

impl AdminV3Fixture {
    async fn boot() -> Self {
        let directory = TempDir::new().unwrap();
        let secret = directory.path().join("provider.key");
        std::fs::write(&secret, "test-secret").unwrap();
        let mut config = ServerConfig::from_toml(&format!(
            r#"
schema_version = 1

[server]
data_dir = "{}"
session_db = "{}"

[[model_providers]]
id = "alpha"
base_url = "https://alpha.invalid"
[model_providers.api_key]
source = "file"
path = "{}"
[[model_providers.models]]
id = "shared"

[[model_providers]]
id = "beta"
base_url = "https://beta.invalid"
[model_providers.api_key]
source = "file"
path = "{}"
[[model_providers.models]]
id = "shared"

[[agents]]
[agents.spec]
id = "assistant"
name = "Assistant"
[agents.spec.model]
provider = "alpha"
model_name = "shared"
"#,
            directory.path().display(),
            directory.path().join("runtime.db").display(),
            secret.display(),
            secret.display(),
        ))
        .unwrap();
        config.agents[0].spec.model.allowed_models = vec![model("alpha")];
        let runtime = Runtime::boot_config(config.clone()).await.unwrap();
        let mut principal =
            AuthenticatedPrincipal::user("operator", AuthenticationMethod::Internal);
        principal.roles.push("admin".into());
        let administrator =
            BoundaryContext::authenticated(principal, "admin-test", "internal", "agent-admin-v3");
        Self {
            _directory: directory,
            runtime,
            config,
            administrator,
        }
    }

    fn registry(&self) -> &crate::agent_registry::AgentRegistry {
        self.runtime.ui_service.agent_registry.as_ref().unwrap()
    }

    async fn update_cross_provider_revision(&self) {
        let mut definition = self.config.agents[0].clone();
        definition.revision = 2;
        definition.spec.model.allowed_models = vec![model("beta"), model("alpha")];
        let response = sylvander_channel::UiService::agent_admin(
            self.runtime.ui_service.as_ref(),
            &self.administrator,
            AgentAdminRequest::UpdateDefinition {
                expected_active_revision: 1,
                definition: Box::new(
                    crate::agent_admin::draft_from_definition(&definition).unwrap(),
                ),
            },
        )
        .await;
        assert!(matches!(
            response,
            AgentAdminResponse::Success { result }
                if matches!(
                    result.as_ref(),
                    AgentAdminResult::DefinitionUpdated { revision }
                        if revision.definition.revision == 2 && !revision.active
                )
        ));
    }

    async fn activate_revision_two(&self) -> AgentAdminResponse {
        sylvander_channel::UiService::agent_admin(
            self.runtime.ui_service.as_ref(),
            &self.administrator,
            AgentAdminRequest::ActivateRevision {
                agent_id: AgentId::new("assistant"),
                revision: 2,
                expected_active_revision: 1,
            },
        )
        .await
    }
}

async fn advance_component_heads(fixture: &AdminV3Fixture) {
    let plan = RegistryBootstrapPlan::from_config(&fixture.config).unwrap();
    for provider_id in ["alpha", "beta"] {
        let mut provider = plan
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .unwrap()
            .clone();
        provider.revision = 2;
        provider.base_url = format!("https://{provider_id}-v2.invalid");
        fixture
            .registry()
            .stage_provider(1, provider)
            .await
            .unwrap();
        fixture
            .registry()
            .activate_provider(provider_id, 2, 1)
            .await
            .unwrap();

        let mut definition = plan
            .models
            .iter()
            .find(|model| model.provider_id == provider_id && model.model_id == "shared")
            .unwrap()
            .clone();
        definition.revision = 2;
        definition.context_window += 1;
        fixture.registry().stage_model(1, definition).await.unwrap();
        fixture
            .registry()
            .activate_model((provider_id, "shared"), 2, 1)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn cross_provider_update_pins_native_v3_and_survives_head_drift() {
    let fixture = AdminV3Fixture::boot().await;
    fixture.update_cross_provider_revision().await;

    let snapshot = fixture
        .registry()
        .load_agent_snapshot_v3("assistant", 2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.providers,
        [("alpha".into(), 1), ("beta".into(), 1)].into()
    );
    assert_eq!(
        snapshot
            .models
            .iter()
            .map(|pin| (pin.model.clone(), pin.revision))
            .collect::<Vec<_>>(),
        vec![(model("alpha"), 1), (model("beta"), 1)]
    );
    assert!(
        fixture
            .registry()
            .load_agent_snapshot("assistant", 2)
            .await
            .unwrap()
            .is_none()
    );

    advance_component_heads(&fixture).await;
    let historical = fixture
        .registry()
        .resolve_registry_composition_versioned(&AgentId::new("assistant"), 2)
        .await
        .unwrap();
    assert!(
        historical
            .providers
            .values()
            .all(|provider| provider.revision == 1)
    );
    assert!(historical.models.values().all(|model| model.revision == 1));
    assert!(matches!(
        fixture.activate_revision_two().await,
        AgentAdminResponse::Success { result }
            if matches!(
                result.as_ref(),
                AgentAdminResult::RevisionActivated { active_revision: 2, .. }
            )
    ));
    let rolled_back = sylvander_channel::UiService::agent_admin(
        fixture.runtime.ui_service.as_ref(),
        &fixture.administrator,
        AgentAdminRequest::RollbackRevision {
            agent_id: AgentId::new("assistant"),
            target_revision: 1,
            expected_active_revision: 2,
        },
    )
    .await;
    assert!(matches!(
        rolled_back,
        AgentAdminResponse::Success { result }
            if matches!(
                result.as_ref(),
                AgentAdminResult::RevisionRolledBack { active_revision: 1, .. }
            )
    ));
    fixture.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn dynamic_registry_catalog_and_active_agent_survive_original_config_restart() {
    let directory = TempDir::new().unwrap();
    let secret = directory.path().join("provider.key");
    std::fs::write(&secret, "test-secret").unwrap();
    let mut config = ServerConfig::from_toml(&format!(
        r#"
schema_version = 1

[server]
data_dir = "{}"
session_db = "{}"

[[model_providers]]
id = "alpha"
base_url = "https://alpha.invalid"
[model_providers.api_key]
source = "file"
path = "{}"
[[model_providers.models]]
id = "shared"

[[agents]]
[agents.spec]
id = "assistant"
name = "Assistant"
[agents.spec.model]
provider = "alpha"
model_name = "shared"
"#,
        directory.path().display(),
        directory.path().join("runtime.db").display(),
        secret.display(),
    ))
    .unwrap();
    config.agents[0].spec.model.allowed_models = vec![model("alpha")];
    let original = config.clone();
    let runtime = Runtime::boot_config(config.clone()).await.unwrap();
    let mut principal = AuthenticatedPrincipal::user("operator", AuthenticationMethod::Internal);
    principal.roles.push("admin".into());
    let administrator =
        BoundaryContext::authenticated(principal, "admin-test", "internal", "dynamic-restart");
    let registry = runtime.ui_service.agent_registry.as_ref().unwrap();
    let binding_id = registry
        .load_active_provider("alpha")
        .await
        .unwrap()
        .unwrap()
        .definition
        .credential_binding_id;

    let provider = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::RegistryAdminRequest::CreateProvider {
            provider_id: "beta".into(),
            definition: sylvander_protocol::ProviderDefinitionDraft {
                kind: "anthropic_compatible".into(),
                base_url: "https://beta.invalid".into(),
                credential_binding_id: binding_id,
            },
        },
    )
    .await;
    assert!(matches!(provider, RegistryAdminResponse::Success { .. }));
    let model_response = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::RegistryAdminRequest::CreateModel {
            provider_id: "beta".into(),
            model_id: "shared".into(),
            definition: sylvander_protocol::ModelDefinitionDraft {
                context_window: 100_000,
                max_output_tokens: 4096,
                capabilities: vec![
                    "vision".into(),
                    "tool_use".into(),
                    "extended_thinking".into(),
                ],
                lifecycle: sylvander_protocol::ModelLifecycleDraft::Deprecated {
                    replacement: Some("alpha/shared".into()),
                },
                pricing: Some(sylvander_protocol::ModelPricingDraft {
                    input_usd_micros_per_million: 11,
                    output_usd_micros_per_million: 29,
                    cache_write_usd_micros_per_million: Some(7),
                    cache_read_usd_micros_per_million: Some(3),
                }),
            },
        },
    )
    .await;
    assert!(matches!(
        model_response,
        RegistryAdminResponse::Success { .. }
    ));

    let mut next = config.agents[0].clone();
    next.revision = 2;
    next.spec.model.allowed_models = vec![model("alpha"), model("beta")];
    let updated = sylvander_channel::UiService::agent_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        AgentAdminRequest::UpdateDefinition {
            expected_active_revision: 1,
            definition: Box::new(crate::agent_admin::draft_from_definition(&next).unwrap()),
        },
    )
    .await;
    assert!(matches!(updated, AgentAdminResponse::Success { .. }));
    assert!(matches!(
        sylvander_channel::UiService::agent_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            AgentAdminRequest::ActivateRevision {
                agent_id: AgentId::new("assistant"),
                revision: 2,
                expected_active_revision: 1,
            },
        )
        .await,
        AgentAdminResponse::Success { .. }
    ));

    let discovered =
        sylvander_channel::UiService::discover_agents(runtime.ui_service.as_ref(), &administrator)
            .await
            .unwrap();
    assert_dynamic_beta_descriptor(&discovered);
    let created = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &administrator,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "dynamic beta".into(),
            channel_id: None,
            overrides: SessionConfigOverrides::default(),
        },
    )
    .await
    .unwrap();
    assert_eq!(created.effective.model_selection(), model("alpha"));
    assert_eq!(created.effective.agent_revision, 2);

    let ambiguous = sylvander_channel::UiService::update_session_config(
        runtime.ui_service.as_ref(),
        &administrator,
        SessionConfigUpdateRequest {
            session_id: created.session_id.clone(),
            expected_revision: created.revision,
            overrides: SessionConfigOverrides {
                model_id: Some("shared".into()),
                ..SessionConfigOverrides::default()
            },
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        ambiguous.code,
        sylvander_protocol::BoundaryErrorCode::InvalidScope
    );
    assert!(ambiguous.message.contains("ambiguous"));
    let unchanged = sylvander_channel::UiService::session_config(
        runtime.ui_service.as_ref(),
        &administrator,
        &created.session_id,
    )
    .await
    .unwrap();
    assert_eq!(unchanged.revision, created.revision);
    assert_eq!(unchanged.overrides, created.overrides);

    let updated = sylvander_channel::UiService::update_session_config(
        runtime.ui_service.as_ref(),
        &administrator,
        SessionConfigUpdateRequest {
            session_id: created.session_id.clone(),
            expected_revision: created.revision,
            overrides: SessionConfigOverrides {
                model: Some(model("beta")),
                ..SessionConfigOverrides::default()
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(updated.effective.model_selection(), model("beta"));
    assert_eq!(updated.effective.agent_revision, 2);
    assert_eq!(updated.effective.provider_revision, Some(1));
    assert_eq!(updated.effective.model_revision, Some(1));
    runtime.shutdown().await.unwrap();

    let restarted = Runtime::boot_config(original).await.unwrap();
    let rediscovered = sylvander_channel::UiService::discover_agents(
        restarted.ui_service.as_ref(),
        &administrator,
    )
    .await
    .unwrap();
    assert_dynamic_beta_descriptor(&rediscovered);
    let restored = sylvander_channel::UiService::session_config(
        restarted.ui_service.as_ref(),
        &administrator,
        &created.session_id,
    )
    .await
    .unwrap();
    assert_eq!(restored.revision, updated.revision);
    assert_eq!(restored.overrides, updated.overrides);
    assert_eq!(restored.effective, updated.effective);
    let active = restarted
        .ui_service
        .agent_registry
        .as_ref()
        .unwrap()
        .load_active(&AgentId::new("assistant"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active.definition.revision, 2);
    assert_eq!(active.definition.spec.model.provider, "alpha");
    let snapshot = restarted
        .ui_service
        .agent_registry
        .as_ref()
        .unwrap()
        .load_agent_snapshot_v3("assistant", 2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.providers,
        [("alpha".into(), 1), ("beta".into(), 1)].into()
    );
    assert_eq!(
        snapshot
            .models
            .iter()
            .map(|pin| (pin.model.clone(), pin.revision))
            .collect::<Vec<_>>(),
        vec![(model("alpha"), 1), (model("beta"), 1)]
    );
    assert_eq!(
        restarted.configured_agents[&AgentId::new("assistant")]
            .spec
            .model
            .provider,
        "alpha"
    );
    restarted.shutdown().await.unwrap();
}

fn assert_dynamic_beta_descriptor(descriptors: &[sylvander_protocol::AgentDescriptor]) {
    let beta = descriptors
        .iter()
        .find(|agent| agent.id == AgentId::new("assistant"))
        .unwrap()
        .models
        .iter()
        .find(|model| model.provider == "beta" && model.id == "shared")
        .unwrap();
    assert_eq!(
        beta.capability_names,
        vec![
            ModelCapability::ExtendedThinking,
            ModelCapability::ToolUse,
            ModelCapability::Vision,
        ]
    );
    assert_eq!(
        beta.lifecycle,
        sylvander_protocol::ModelLifecycle::Deprecated {
            replacement: Some("alpha/shared".into())
        }
    );
    assert_eq!(
        beta.pricing,
        Some(sylvander_protocol::ModelPricing {
            input_usd_micros_per_million: 11,
            output_usd_micros_per_million: 29,
            cache_write_usd_micros_per_million: Some(7),
            cache_read_usd_micros_per_million: Some(3),
        })
    );
}

enum Corruption {
    Credential,
    Model,
}

#[tokio::test]
async fn activation_revalidates_cached_revision_and_preserves_active_head_on_corruption() {
    for corruption in [Corruption::Credential, Corruption::Model] {
        let fixture = AdminV3Fixture::boot().await;
        fixture.update_cross_provider_revision().await;
        match corruption {
            Corruption::Credential => {
                let binding_id = fixture
                    .registry()
                    .load_active_provider("beta")
                    .await
                    .unwrap()
                    .unwrap()
                    .definition
                    .credential_binding_id;
                fixture
                    .registry()
                    .run(move |connection| {
                        connection
                            .execute_batch("PRAGMA foreign_keys=OFF;")
                            .map_err(AgentRegistryError::sqlite)?;
                        connection
                            .execute(
                                "DELETE FROM credential_binding_heads WHERE binding_id=?1",
                                [binding_id],
                            )
                            .map_err(AgentRegistryError::sqlite)?;
                        connection
                            .execute_batch("PRAGMA foreign_keys=ON;")
                            .map_err(AgentRegistryError::sqlite)?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
            Corruption::Model => {
                fixture
                    .registry()
                    .run(|connection| {
                        connection
                            .execute_batch(
                                "PRAGMA foreign_keys=OFF; \
                                 DELETE FROM model_definitions WHERE provider_id='beta' \
                                 AND model_id='shared' AND revision=1; \
                                 PRAGMA foreign_keys=ON;",
                            )
                            .map_err(AgentRegistryError::sqlite)
                    })
                    .await
                    .unwrap();
            }
        }

        assert!(matches!(
            fixture.activate_revision_two().await,
            AgentAdminResponse::Error { error }
                if error.code == AgentAdminErrorCode::InvalidDefinition
        ));
        assert_eq!(
            fixture
                .registry()
                .load_active(&AgentId::new("assistant"))
                .await
                .unwrap()
                .unwrap()
                .definition
                .revision,
            1
        );
        fixture.runtime.shutdown().await.unwrap();
    }
}
