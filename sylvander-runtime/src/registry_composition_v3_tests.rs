use std::collections::BTreeSet;

use sylvander_protocol::ModelSelection;
use tempfile::tempdir;

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::agent_registry_snapshot::AgentSnapshotSelection;
use crate::agent_registry_snapshot_v3::AgentSnapshotSelectionV3;
use crate::config::ServerConfig;
use crate::registry_composition_v3::VersionedRegistryCompositionError;

fn model(provider_id: &str) -> ModelSelection {
    ModelSelection {
        provider_id: provider_id.into(),
        model_id: "shared".into(),
    }
}

fn config() -> ServerConfig {
    ServerConfig::from_toml(
        r#"
schema_version = 1

[[model_providers]]
id = "alpha"
base_url = "https://alpha.invalid"
[model_providers.api_key]
source = "env"
name = "ALPHA_KEY"
[[model_providers.models]]
id = "shared"

[[model_providers]]
id = "beta"
base_url = "https://beta.invalid"
[model_providers.api_key]
source = "env"
name = "BETA_KEY"
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
    )
    .unwrap()
}

fn v3_selection(default_provider: &str) -> AgentSnapshotSelectionV3 {
    AgentSnapshotSelectionV3 {
        agent_id: "assistant".into(),
        agent_revision: 1,
        default_model: model(default_provider),
        allowed_models: BTreeSet::from([model("beta"), model("alpha")]),
    }
}

async fn install(registry: &AgentRegistry) -> ServerConfig {
    let config = config();
    registry.bootstrap_registries(&config).await.unwrap();
    registry.seed(&config).await.unwrap();
    config
}

#[tokio::test]
async fn native_v3_composes_same_model_id_from_two_exact_providers() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    registry
        .stage_agent_snapshot_v3(v3_selection("alpha"))
        .await
        .unwrap();

    let composed = registry
        .resolve_registry_composition_versioned(&config.agents[0].spec.id, 1)
        .await
        .unwrap();
    assert_eq!(composed.agent.revision, 1);
    assert_eq!(composed.default_model, model("alpha"));
    assert_eq!(
        composed
            .providers
            .iter()
            .map(|(id, provider)| (id.as_str(), provider.revision))
            .collect::<Vec<_>>(),
        vec![("alpha", 1), ("beta", 1)]
    );
    assert_eq!(
        composed.models.keys().cloned().collect::<Vec<_>>(),
        vec![model("alpha"), model("beta")]
    );
    assert!(
        composed
            .models
            .values()
            .all(|definition| definition.revision == 1)
    );
}

#[tokio::test]
async fn legacy_v2_snapshot_composes_through_the_versioned_loader() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    registry
        .stage_agent_snapshot(AgentSnapshotSelection {
            agent_id: "assistant".into(),
            agent_revision: 1,
            provider_id: "alpha".into(),
            allowed_model_ids: BTreeSet::from(["shared".into()]),
            default_model_id: "shared".into(),
        })
        .await
        .unwrap();

    let composed = registry
        .resolve_registry_composition_versioned(&config.agents[0].spec.id, 1)
        .await
        .unwrap();
    assert_eq!(composed.default_model, model("alpha"));
    assert_eq!(composed.providers.keys().collect::<Vec<_>>(), vec!["alpha"]);
    assert_eq!(
        composed.models.keys().cloned().collect::<Vec<_>>(),
        vec![model("alpha")]
    );
}

async fn composition_after_corruption(sql: &'static str) -> VersionedRegistryCompositionError {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    registry
        .stage_agent_snapshot_v3(v3_selection("alpha"))
        .await
        .unwrap();
    registry
        .run(move |connection| {
            connection
                .execute_batch(sql)
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    registry
        .resolve_registry_composition_versioned(&config.agents[0].spec.id, 1)
        .await
        .unwrap_err()
}

#[tokio::test]
async fn any_missing_or_tampered_component_fails_the_whole_composition() {
    let provider = composition_after_corruption(
        "UPDATE provider_definitions SET digest='tampered' \
         WHERE provider_id='beta' AND revision=1",
    )
    .await;
    assert!(matches!(
        provider,
        VersionedRegistryCompositionError::Registry(AgentRegistryError::Integrity(_))
    ));

    let model_error = composition_after_corruption(
        "PRAGMA foreign_keys=OFF; \
         DELETE FROM model_definitions \
         WHERE provider_id='beta' AND model_id='shared' AND revision=1; \
         PRAGMA foreign_keys=ON;",
    )
    .await;
    assert!(matches!(
        model_error,
        VersionedRegistryCompositionError::UnknownModelRevision { model: missing, revision: 1 }
            if missing == model("beta")
    ));

    let credential = composition_after_corruption(
        "PRAGMA foreign_keys=OFF; \
         DELETE FROM credential_binding_heads \
         WHERE binding_id='provider:beta:api_key'; \
         PRAGMA foreign_keys=ON;",
    )
    .await;
    assert!(matches!(
        credential,
        VersionedRegistryCompositionError::MissingActiveCredentialBinding {
            provider_id,
            binding_id
        } if provider_id == "beta" && binding_id == "provider:beta:api_key"
    ));
}

#[tokio::test]
async fn snapshot_default_pair_must_match_the_agent_default_pair() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    registry
        .stage_agent_snapshot_v3(v3_selection("beta"))
        .await
        .unwrap();

    assert!(matches!(
        registry
            .resolve_registry_composition_versioned(&config.agents[0].spec.id, 1)
            .await,
        Err(VersionedRegistryCompositionError::DefaultModelMismatch {
            configured,
            snapshot
        }) if configured == model("alpha") && snapshot == model("beta")
    ));
}
