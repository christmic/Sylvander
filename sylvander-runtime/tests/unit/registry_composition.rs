use std::collections::BTreeSet;

use tempfile::tempdir;

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::agent_registry_snapshot::AgentSnapshotSelection;
use crate::config::ServerConfig;
use crate::registry_bootstrap::RegistryBootstrapPlan;
use crate::registry_composition::RegistryCompositionError;

fn config() -> ServerConfig {
    ServerConfig::from_toml(
        r#"
schema_version = 1
[[model_providers]]
id = "alpha"
base_url = "https://alpha-v1.invalid"
[model_providers.api_key]
source = "env"
name = "ALPHA_SECRET_REFERENCE"
[[model_providers.models]]
id = "alternate"
context_window = 100
[[model_providers.models]]
id = "shared"
context_window = 200
[[agents]]
[agents.spec]
id = "assistant"
name = "Assistant"
[agents.spec.model]
provider = "alpha"
model_name = "shared"
"#,
    )
    .unwrap()
}

fn selection(revision: u64) -> AgentSnapshotSelection {
    AgentSnapshotSelection {
        agent_id: "assistant".into(),
        agent_revision: revision,
        provider_id: "alpha".into(),
        allowed_model_ids: BTreeSet::from(["alternate".into(), "shared".into()]),
        default_model_id: "shared".into(),
    }
}

async fn install(registry: &AgentRegistry) -> ServerConfig {
    let config = config();
    registry.bootstrap_registries(&config).await.unwrap();
    registry.seed(&config).await.unwrap();
    registry.stage_agent_snapshot(selection(1)).await.unwrap();
    config
}

async fn advance(registry: &AgentRegistry, config: &ServerConfig) {
    let plan = RegistryBootstrapPlan::from_config(config).unwrap();
    let mut provider = plan.providers[0].clone();
    provider.revision = 2;
    provider.base_url = "https://alpha-v2.invalid".into();
    registry.stage_provider(1, provider).await.unwrap();
    registry.activate_provider("alpha", 2, 1).await.unwrap();
    for mut model in plan.models.clone() {
        model.revision = 2;
        model.context_window += 10;
        registry.stage_model(1, model.clone()).await.unwrap();
        registry
            .activate_model((&model.provider_id, &model.model_id), 2, 1)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn composition_uses_exact_revisions_without_following_heads() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    let old = registry
        .resolve_registry_composition(&config.agents[0].spec.id, 1)
        .await
        .unwrap();
    assert_eq!(old.provider.revision, 1);
    assert_eq!(old.provider.base_url, "https://alpha-v1.invalid");
    assert!(old.models.iter().all(|model| model.revision == 1));
    assert_eq!(old.default_model_id, "shared");
    assert_eq!(old.credential_binding_id, "provider:alpha:api_key");

    advance(&registry, &config).await;
    let still_old = registry
        .resolve_registry_composition(&config.agents[0].spec.id, 1)
        .await
        .unwrap();
    assert_eq!(still_old.agent.revision, old.agent.revision);
    assert_eq!(still_old.provider, old.provider);
    assert_eq!(still_old.models, old.models);
    assert_eq!(still_old.default_model_id, old.default_model_id);

    let mut definition = config.agents[0].clone();
    definition.revision = 2;
    registry.update(&config, 1, definition).await.unwrap();
    registry.stage_agent_snapshot(selection(2)).await.unwrap();
    let new = registry
        .resolve_registry_composition(&config.agents[0].spec.id, 2)
        .await
        .unwrap();
    assert_eq!(new.provider.revision, 2);
    assert_eq!(new.provider.base_url, "https://alpha-v2.invalid");
    assert!(new.models.iter().all(|model| model.revision == 2));
}

#[tokio::test]
async fn missing_snapshot_model_and_credential_fail_typed_and_redacted() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    let mut definition = config.agents[0].clone();
    definition.revision = 2;
    registry.update(&config, 1, definition).await.unwrap();
    assert!(matches!(
        registry
            .resolve_registry_composition(&config.agents[0].spec.id, 2)
            .await,
        Err(RegistryCompositionError::MissingSnapshot { revision: 2, .. })
    ));

    registry
        .run(|connection| {
            connection
                .execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM model_definitions WHERE provider_id='alpha' AND model_id='shared'; PRAGMA foreign_keys=ON;")
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        registry
            .resolve_registry_composition(&config.agents[0].spec.id, 1)
            .await,
        Err(RegistryCompositionError::UnknownModelRevision { model_id, .. }) if model_id == "shared"
    ));

    let registry = AgentRegistry::open(directory.path().join("credential.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    registry
        .run(|connection| {
            connection
                .execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM credential_binding_heads; PRAGMA foreign_keys=ON;")
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    let error = registry
        .resolve_registry_composition(&config.agents[0].spec.id, 1)
        .await
        .unwrap_err();
    assert!(matches!(
        &error,
        RegistryCompositionError::MissingCredentialBinding(_)
    ));
    assert!(!error.to_string().contains("ALPHA_SECRET_REFERENCE"));
}

#[tokio::test]
async fn tampered_snapshot_fails_before_definition_composition() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    registry
        .run(|connection| {
            connection
                .execute(
                    "UPDATE agent_registry_snapshots SET digest='tampered' WHERE agent_id='assistant'",
                    [],
                )
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    let error = registry
        .resolve_registry_composition(&config.agents[0].spec.id, 1)
        .await
        .unwrap_err();
    assert!(matches!(error, RegistryCompositionError::Snapshot(_)));
}
