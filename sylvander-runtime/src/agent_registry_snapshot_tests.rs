use std::collections::BTreeSet;

use tempfile::tempdir;

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::agent_registry_snapshot::{AgentSnapshotError, AgentSnapshotSelection};
use crate::config::ServerConfig;
use crate::registry_bootstrap::RegistryBootstrapPlan;

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
id = "alternate"
[[model_providers.models]]
id = "shared"

[[model_providers]]
id = "beta"
base_url = "https://beta.invalid"
[model_providers.api_key]
source = "env"
name = "BETA_KEY"
[[model_providers.models]]
id = "alternate"
[[model_providers.models]]
id = "shared"

[[agents]]
[agents.spec]
id = "alpha-agent"
name = "Alpha"
[agents.spec.model]
provider = "alpha"
model_name = "shared"

[[agents]]
[agents.spec]
id = "beta-agent"
name = "Beta"
[agents.spec.model]
provider = "beta"
model_name = "shared"
"#,
    )
    .unwrap()
}

fn selection(agent: &str, revision: u64, provider: &str) -> AgentSnapshotSelection {
    AgentSnapshotSelection {
        agent_id: agent.into(),
        agent_revision: revision,
        provider_id: provider.into(),
        allowed_model_ids: BTreeSet::from(["shared".into()]),
        default_model_id: "shared".into(),
    }
}

async fn install(registry: &AgentRegistry) -> ServerConfig {
    let config = config();
    registry.bootstrap_registries(&config).await.unwrap();
    registry.seed(&config).await.unwrap();
    config
}

async fn advance_alpha_heads(registry: &AgentRegistry, config: &ServerConfig) {
    let plan = RegistryBootstrapPlan::from_config(config).unwrap();
    let mut provider = plan
        .providers
        .iter()
        .find(|provider| provider.id == "alpha")
        .unwrap()
        .clone();
    provider.revision = 2;
    provider.base_url = "https://alpha-v2.invalid".into();
    registry.stage_provider(1, provider).await.unwrap();
    registry.activate_provider("alpha", 2, 1).await.unwrap();
    for mut model in plan
        .models
        .iter()
        .filter(|model| model.provider_id == "alpha")
        .cloned()
    {
        model.revision = 2;
        model.context_window += 1;
        registry.stage_model(1, model.clone()).await.unwrap();
        registry
            .activate_model((&model.provider_id, &model.model_id), 2, 1)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn old_snapshot_stays_pinned_and_new_agent_revision_sees_new_heads() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    let first = registry
        .stage_agent_snapshot(selection("alpha-agent", 1, "alpha"))
        .await
        .unwrap();
    assert_eq!(first.provider_revision, 1);
    assert_eq!(first.models[0].revision, 1);
    assert_eq!(
        registry
            .stage_agent_snapshot(selection("alpha-agent", 1, "alpha"))
            .await
            .unwrap(),
        first
    );

    advance_alpha_heads(&registry, &config).await;
    assert!(matches!(
        registry
            .stage_agent_snapshot(selection("alpha-agent", 1, "alpha"))
            .await,
        Err(AgentSnapshotError::SnapshotCollision { revision: 1, .. })
    ));
    assert_eq!(
        registry
            .load_agent_snapshot("alpha-agent", 1)
            .await
            .unwrap()
            .unwrap(),
        first
    );

    let mut revision_two = config.agents[0].clone();
    revision_two.revision = 2;
    registry.update(&config, 1, revision_two).await.unwrap();
    let second = registry
        .stage_agent_snapshot(selection("alpha-agent", 2, "alpha"))
        .await
        .unwrap();
    assert_eq!(second.provider_revision, 2);
    assert!(second.models.iter().all(|model| model.revision == 2));
}

#[tokio::test]
async fn qualified_models_are_isolated_and_default_is_exactly_one() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    install(&registry).await;
    let alpha = registry
        .stage_agent_snapshot(selection("alpha-agent", 1, "alpha"))
        .await
        .unwrap();
    let mut beta_selection = selection("beta-agent", 1, "beta");
    beta_selection.allowed_model_ids.insert("alternate".into());
    let beta = registry.stage_agent_snapshot(beta_selection).await.unwrap();
    assert_eq!(alpha.models[0].model_id, beta.models[1].model_id);
    assert_ne!(alpha.models[0].provider_id, beta.models[1].provider_id);
    assert_eq!(
        beta.models.iter().filter(|model| model.is_default).count(),
        1
    );

    let mut invalid = selection("alpha-agent", 1, "alpha");
    invalid.default_model_id = "missing".into();
    assert!(matches!(
        registry.stage_agent_snapshot(invalid).await,
        Err(AgentSnapshotError::DefaultNotAllowed(model)) if model == "missing"
    ));
    assert!(
        registry
            .run(|connection| connection
                .execute(
                    "UPDATE agent_registry_snapshot_models SET is_default=1 \
                     WHERE agent_id='beta-agent' AND model_id='alternate'",
                    [],
                )
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn foreign_keys_protect_pins_and_digest_detects_tampering() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    registry
        .stage_agent_snapshot(selection("alpha-agent", 1, "alpha"))
        .await
        .unwrap();
    advance_alpha_heads(&registry, &config).await;

    for statement in [
        "DELETE FROM provider_definitions WHERE provider_id='alpha' AND revision=1",
        "DELETE FROM model_definitions WHERE provider_id='alpha' AND model_id='shared' AND revision=1",
    ] {
        assert!(
            registry
                .run(move |connection| connection
                    .execute(statement, [])
                    .map(|_| ())
                    .map_err(AgentRegistryError::sqlite))
                .await
                .is_err()
        );
    }
    registry
        .run(|connection| {
            connection
                .execute(
                    "UPDATE agent_registry_snapshot_models SET model_revision=2 \
                     WHERE agent_id='alpha-agent' AND model_id='shared'",
                    [],
                )
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        registry.load_agent_snapshot("alpha-agent", 1).await,
        Err(AgentSnapshotError::Integrity(message)) if message.contains("digest mismatch")
    ));
}
