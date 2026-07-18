use std::collections::BTreeSet;

use sylvander_protocol::ModelSelection;
use tempfile::tempdir;

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::agent_registry_snapshot_v3::{AgentSnapshotSelectionV3, AgentSnapshotV3Error};
use crate::config::ServerConfig;
use crate::registry_bootstrap::RegistryBootstrapPlan;

fn model(provider_id: &str, model_id: &str) -> ModelSelection {
    ModelSelection {
        provider_id: provider_id.into(),
        model_id: model_id.into(),
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
allowed_models = [
  { provider_id = "alpha", model_id = "shared" },
  { provider_id = "beta", model_id = "shared" },
]
"#,
    )
    .unwrap()
}

fn selection() -> AgentSnapshotSelectionV3 {
    AgentSnapshotSelectionV3 {
        agent_id: "assistant".into(),
        agent_revision: 1,
        default_model: model("alpha", "shared"),
        allowed_models: BTreeSet::from([model("beta", "shared"), model("alpha", "shared")]),
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

    let mut model = plan
        .models
        .iter()
        .find(|model| model.provider_id == "alpha" && model.model_id == "shared")
        .unwrap()
        .clone();
    model.revision = 2;
    model.context_window += 1;
    registry.stage_model(1, model.clone()).await.unwrap();
    registry
        .activate_model((&model.provider_id, &model.model_id), 2, 1)
        .await
        .unwrap();
}

fn revision_two(
    config: &ServerConfig,
    allowed_models: BTreeSet<ModelSelection>,
    default_model: ModelSelection,
) -> (
    crate::config::AgentDefinitionConfig,
    AgentSnapshotSelectionV3,
) {
    let mut definition = config.agents[0].clone();
    definition.revision = 2;
    definition.spec.model.provider = default_model.provider_id.clone();
    definition.spec.model.model_name = default_model.model_id.clone();
    definition.spec.model.allowed_models = allowed_models.iter().cloned().collect();
    let selection = AgentSnapshotSelectionV3 {
        agent_id: definition.spec.id.to_string(),
        agent_revision: definition.revision,
        default_model,
        allowed_models,
    };
    (definition, selection)
}

#[tokio::test]
async fn atomic_stage_pins_one_exact_active_catalog() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    advance_alpha_heads(&registry, &config).await;
    let (definition, selection) = revision_two(
        &config,
        BTreeSet::from([model("alpha", "shared"), model("beta", "shared")]),
        model("alpha", "shared"),
    );

    let (stored, snapshot) = registry
        .stage_agent_revision_v3(1, definition, selection)
        .await
        .unwrap();

    assert_eq!(stored.definition.revision, 2);
    assert!(!stored.active);
    assert_eq!(
        snapshot.providers,
        [("alpha".into(), 2), ("beta".into(), 1)].into()
    );
    assert_eq!(
        snapshot
            .models
            .iter()
            .map(|pin| (pin.model.clone(), pin.revision))
            .collect::<Vec<_>>(),
        vec![(model("alpha", "shared"), 2), (model("beta", "shared"), 1)]
    );
    assert_eq!(
        registry
            .load_agent_snapshot_v3("assistant", 2)
            .await
            .unwrap(),
        Some(snapshot)
    );
}

#[tokio::test]
async fn atomic_stage_unknown_model_rolls_back_definition_and_snapshot() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    let missing = model("alpha", "missing");
    let (definition, selection) =
        revision_two(&config, BTreeSet::from([missing.clone()]), missing.clone());

    assert!(matches!(
        registry
            .stage_agent_revision_v3(1, definition, selection)
            .await,
        Err(AgentSnapshotV3Error::UnknownModel { provider_id, model_id })
            if provider_id == "alpha" && model_id == "missing"
    ));
    assert!(
        registry
            .load(&sylvander_protocol::AgentId::new("assistant"), 2)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        registry
            .load_agent_snapshot_v3("assistant", 2)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn atomic_stage_cas_conflict_leaves_no_revision_or_snapshot() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    let (definition, selection) = revision_two(
        &config,
        BTreeSet::from([model("alpha", "shared")]),
        model("alpha", "shared"),
    );

    assert!(matches!(
        registry
            .stage_agent_revision_v3(99, definition, selection)
            .await,
        Err(AgentSnapshotV3Error::Registry(
            AgentRegistryError::Conflict {
                expected: 99,
                actual: 1,
                ..
            }
        ))
    ));
    assert!(
        registry
            .load(&sylvander_protocol::AgentId::new("assistant"), 2)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        registry
            .load_agent_snapshot_v3("assistant", 2)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn multi_provider_snapshot_is_exact_idempotent_and_survives_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let registry = AgentRegistry::open(&path).await.unwrap();
    install(&registry).await;

    let first = registry.stage_agent_snapshot_v3(selection()).await.unwrap();
    assert_eq!(
        first.providers,
        [("alpha".into(), 1), ("beta".into(), 1)].into()
    );
    assert_eq!(
        first
            .models
            .iter()
            .map(|pin| (pin.model.clone(), pin.revision))
            .collect::<Vec<_>>(),
        vec![(model("alpha", "shared"), 1), (model("beta", "shared"), 1)]
    );
    assert_eq!(
        registry.stage_agent_snapshot_v3(selection()).await.unwrap(),
        first
    );
    let row_counts = registry
        .run(|connection| {
            let count = |table: &str| -> Result<i64, AgentRegistryError> {
                connection
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE agent_id='assistant'"),
                        [],
                        |row| row.get(0),
                    )
                    .map_err(AgentRegistryError::sqlite)
            };
            Ok((
                count("agent_registry_snapshots_v3")?,
                count("agent_registry_snapshot_providers_v3")?,
                count("agent_registry_snapshot_models_v3")?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(row_counts, (1, 2, 2));

    drop(registry);
    let reopened = AgentRegistry::open(path).await.unwrap();
    assert_eq!(
        reopened
            .load_agent_snapshot_v3("assistant", 1)
            .await
            .unwrap(),
        Some(first)
    );
}

#[tokio::test]
async fn same_revision_rejects_changed_component_heads() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = install(&registry).await;
    registry.stage_agent_snapshot_v3(selection()).await.unwrap();
    advance_alpha_heads(&registry, &config).await;

    assert!(matches!(
        registry.stage_agent_snapshot_v3(selection()).await,
        Err(AgentSnapshotV3Error::SnapshotCollision {
            agent_id,
            revision: 1
        }) if agent_id == "assistant"
    ));
}

#[tokio::test]
async fn damaged_current_snapshot_fails_without_fallback() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    install(&registry).await;
    registry.stage_agent_snapshot_v3(selection()).await.unwrap();
    registry
        .run(|connection| {
            connection
                .execute(
                    "UPDATE agent_registry_snapshots_v3 SET digest='tampered' \
                     WHERE agent_id='assistant' AND agent_revision=1",
                    [],
                )
                .map_err(AgentRegistryError::sqlite)?;
            Ok(())
        })
        .await
        .unwrap();

    assert!(matches!(
        registry.load_agent_snapshot_versioned("assistant", 1).await,
        Err(AgentSnapshotV3Error::Registry(
            AgentRegistryError::Integrity(_)
        ))
    ));
}

#[tokio::test]
async fn current_loader_rejects_a_mixed_schema_added_after_open() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    install(&registry).await;
    registry.stage_agent_snapshot_v3(selection()).await.unwrap();
    registry
        .run(|connection| {
            connection
                .execute_batch(
                    "CREATE TABLE agent_registry_snapshots (
                        agent_id TEXT NOT NULL,
                        agent_revision INTEGER NOT NULL,
                        provider_id TEXT NOT NULL,
                        provider_revision INTEGER NOT NULL,
                        digest TEXT NOT NULL,
                        created_at INTEGER NOT NULL,
                        PRIMARY KEY(agent_id,agent_revision)
                    );",
                )
                .map_err(AgentRegistryError::sqlite)?;
            Ok(())
        })
        .await
        .unwrap();

    assert!(matches!(
        registry.load_agent_snapshot_versioned("assistant", 1).await,
        Err(AgentSnapshotV3Error::Registry(
            AgentRegistryError::Integrity(_)
        ))
    ));
}
