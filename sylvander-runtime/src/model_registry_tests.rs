use std::collections::BTreeSet;

use rusqlite::params;
use sylvander_protocol::ModelLifecycle;
use tempfile::tempdir;

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::config::SecretRef;
use crate::model_registry::ModelRegistryError;
use crate::registry_domain::{
    ModelDefinition, ProviderDefinition, canonical_definition, canonical_secret_reference,
};

fn model(provider_id: &str, revision: u64, context_window: u32) -> ModelDefinition {
    ModelDefinition {
        provider_id: provider_id.into(),
        model_id: "shared".into(),
        revision,
        context_window,
        max_output_tokens: 1024,
        capabilities: BTreeSet::from(["tool_use".into()]),
        lifecycle: ModelLifecycle::Active,
        pricing: None,
    }
}

async fn install_providers(registry: &AgentRegistry) {
    let (reference, credential_digest) = canonical_secret_reference(&SecretRef::Env {
        name: "PROVIDER_API_KEY".into(),
    })
    .unwrap();
    let providers = ["alpha", "beta"].map(|id| {
        let definition = ProviderDefinition {
            id: id.into(),
            revision: 1,
            kind: "anthropic_compatible".into(),
            base_url: format!("https://{id}.invalid"),
            credential_binding_id: "credential/main".into(),
        };
        let encoded = canonical_definition(&definition).unwrap();
        (definition, encoded)
    });
    registry
        .run(move |connection| {
            connection
                .execute(
                    "INSERT INTO credential_binding_revisions VALUES (?1,1,?2,?3,1)",
                    params!["credential/main", reference, credential_digest],
                )
                .map_err(AgentRegistryError::sqlite)?;
            connection
                .execute(
                    "INSERT INTO credential_binding_heads VALUES (?1,1,1)",
                    ["credential/main"],
                )
                .map_err(AgentRegistryError::sqlite)?;
            for (definition, encoded) in providers {
                connection
                    .execute(
                        "INSERT INTO provider_definitions VALUES (?1,1,?2,?3,?4,1)",
                        params![
                            definition.id,
                            encoded.0,
                            encoded.1,
                            definition.credential_binding_id
                        ],
                    )
                    .map_err(AgentRegistryError::sqlite)?;
                connection
                    .execute(
                        "INSERT INTO provider_registry_heads VALUES (?1,1,1)",
                        [definition.id],
                    )
                    .map_err(AgentRegistryError::sqlite)?;
            }
            Ok(())
        })
        .await
        .unwrap();
}

fn assert_one_conflict(
    results: (
        Result<(), ModelRegistryError>,
        Result<(), ModelRegistryError>,
    ),
) {
    let values = [results.0, results.1];
    assert_eq!(values.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        values
            .iter()
            .filter(|result| matches!(result, Err(ModelRegistryError::Conflict { .. })))
            .count(),
        1
    );
}

#[tokio::test]
async fn model_lifecycle_preserves_qualified_identity_and_provider_head() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let registry = AgentRegistry::open(&path).await.unwrap();
    assert!(matches!(
        registry.seed_model(model("missing", 1, 10)).await,
        Err(ModelRegistryError::UnknownProvider(provider)) if provider == "missing"
    ));
    install_providers(&registry).await;

    let alpha = registry.seed_model(model("alpha", 1, 100)).await.unwrap();
    assert!(alpha.active);
    assert_eq!(
        registry
            .seed_model(model("alpha", 1, 999))
            .await
            .unwrap()
            .definition
            .context_window,
        100
    );
    registry.seed_model(model("beta", 1, 200)).await.unwrap();
    assert_eq!(
        registry
            .load_active_model(("beta", "shared"))
            .await
            .unwrap()
            .unwrap()
            .definition
            .context_window,
        200
    );

    let staged = registry
        .stage_model(1, model("alpha", 2, 120))
        .await
        .unwrap();
    assert!(!staged.active);
    assert_eq!(
        registry
            .stage_model(1, model("alpha", 2, 120))
            .await
            .unwrap()
            .digest,
        staged.digest
    );
    assert!(matches!(
        registry.stage_model(1, model("alpha", 2, 121)).await,
        Err(ModelRegistryError::RevisionCollision { revision: 2, .. })
    ));
    registry
        .stage_model(1, model("alpha", 3, 130))
        .await
        .unwrap();
    registry
        .activate_model(("alpha", "shared"), 2, 1)
        .await
        .unwrap();
    assert!(matches!(
        registry.activate_model(("alpha", "shared"), 3, 1).await,
        Err(ModelRegistryError::Conflict {
            expected: 1,
            actual: 2,
            ..
        })
    ));

    assert!(
        registry
            .run(|connection| connection
                .execute(
                    "DELETE FROM provider_registry_heads WHERE provider_id='alpha'",
                    [],
                )
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite))
            .await
            .is_err()
    );
    drop(registry);
    let registry = AgentRegistry::open(path).await.unwrap();
    let revisions = registry.inspect_model(("alpha", "shared")).await.unwrap();
    assert_eq!(
        revisions
            .iter()
            .map(|item| item.definition.revision)
            .collect::<Vec<_>>(),
        [3, 2, 1]
    );
    assert_eq!(revisions.iter().filter(|item| item.active).count(), 1);
    registry
        .rollback_model(("alpha", "shared"), 1, 2)
        .await
        .unwrap();
    assert!(matches!(
        registry.activate_model(("alpha", "shared"), 99, 1).await,
        Err(ModelRegistryError::UnknownRevision { revision: 99, .. })
    ));
}

#[tokio::test]
async fn two_connections_allow_only_one_model_activation_and_rollback() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let first = AgentRegistry::open(&path).await.unwrap();
    install_providers(&first).await;
    let second = AgentRegistry::open(&path).await.unwrap();
    first.seed_model(model("alpha", 1, 100)).await.unwrap();
    first.stage_model(1, model("alpha", 2, 120)).await.unwrap();
    first.stage_model(1, model("alpha", 3, 130)).await.unwrap();

    assert_one_conflict(tokio::join!(
        first.activate_model(("alpha", "shared"), 2, 1),
        second.activate_model(("alpha", "shared"), 3, 1)
    ));
    let active = first
        .load_active_model(("alpha", "shared"))
        .await
        .unwrap()
        .unwrap()
        .definition
        .revision;
    if active == 2 {
        first
            .activate_model(("alpha", "shared"), 3, 2)
            .await
            .unwrap();
    }
    assert_one_conflict(tokio::join!(
        first.rollback_model(("alpha", "shared"), 1, 3),
        second.rollback_model(("alpha", "shared"), 2, 3)
    ));
    assert!(matches!(
        first.rollback_model(("alpha", "shared"), 1, 3).await,
        Err(ModelRegistryError::Conflict { expected: 3, .. })
    ));
}
