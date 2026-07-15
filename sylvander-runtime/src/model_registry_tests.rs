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

#[tokio::test]
async fn bounded_model_pages_preserve_qualified_identity_across_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let registry = AgentRegistry::open(&path).await.unwrap();
    install_providers(&registry).await;
    registry.seed_model(model("alpha", 1, 100)).await.unwrap();
    registry.seed_model(model("beta", 1, 900)).await.unwrap();
    for revision in 2..=5 {
        registry
            .stage_model(1, model("alpha", revision, 100 + revision as u32))
            .await
            .unwrap();
    }
    registry
        .activate_model(("alpha", "shared"), 4, 1)
        .await
        .unwrap();

    let first = registry
        .inspect_model_page(("alpha", "shared"), None, 2)
        .await
        .unwrap();
    assert_eq!(first.active_revision, 4);
    assert_eq!(
        first
            .revisions
            .iter()
            .map(|stored| stored.definition.revision)
            .collect::<Vec<_>>(),
        [5, 4]
    );
    assert!(first.revisions[1].active);
    assert!(first.revisions.iter().all(|stored| {
        stored.definition.provider_id == "alpha" && stored.definition.model_id == "shared"
    }));
    assert_eq!(first.next_before_revision, Some(4));
    drop(registry);

    let reopened = AgentRegistry::open(path).await.unwrap();
    let second = reopened
        .inspect_model_page(("alpha", "shared"), first.next_before_revision, 2)
        .await
        .unwrap();
    assert_eq!(second.active_revision, 4);
    assert_eq!(
        second
            .revisions
            .iter()
            .map(|stored| stored.definition.revision)
            .collect::<Vec<_>>(),
        [3, 2]
    );
    assert_eq!(second.next_before_revision, Some(2));
    let final_page = reopened
        .inspect_model_page(("alpha", "shared"), Some(2), 2)
        .await
        .unwrap();
    assert_eq!(final_page.revisions[0].definition.revision, 1);
    assert_eq!(final_page.next_before_revision, None);
    let beta = reopened
        .inspect_model_page(("beta", "shared"), None, 2)
        .await
        .unwrap();
    assert_eq!(beta.revisions.len(), 1);
    assert_eq!(beta.revisions[0].definition.context_window, 900);
}

#[tokio::test]
async fn bounded_model_page_distinguishes_unknown_and_integrity_failures() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    install_providers(&registry).await;
    assert!(matches!(
        registry
            .inspect_model_page(("alpha", "unknown"), None, 10)
            .await,
        Err(ModelRegistryError::UnknownModel(identity)) if identity == "alpha/unknown"
    ));
    registry.seed_model(model("alpha", 1, 100)).await.unwrap();
    registry
        .run(|connection| {
            connection
                .execute(
                    "DELETE FROM model_registry_heads WHERE provider_id='alpha' AND model_id='shared'",
                    [],
                )
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        registry
            .inspect_model_page(("alpha", "shared"), None, 10)
            .await,
        Err(ModelRegistryError::Registry(AgentRegistryError::Integrity(
            _
        )))
    ));

    let registry = AgentRegistry::open(directory.path().join("tampered.db"))
        .await
        .unwrap();
    install_providers(&registry).await;
    registry.seed_model(model("alpha", 1, 100)).await.unwrap();
    registry
        .run(|connection| {
            connection
                .execute("UPDATE model_definitions SET digest='tampered'", [])
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        registry
            .inspect_model_page(("alpha", "shared"), None, 10)
            .await,
        Err(ModelRegistryError::Registry(AgentRegistryError::Integrity(
            _
        )))
    ));
}
