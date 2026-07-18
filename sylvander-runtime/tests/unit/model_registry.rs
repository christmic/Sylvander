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

fn assert_one_conflict<T>(results: (Result<T, ModelRegistryError>, Result<T, ModelRegistryError>)) {
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

async fn model_row_counts(registry: &AgentRegistry) -> (i64, i64) {
    registry
        .run(|connection| {
            connection
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM model_definitions \
                             WHERE provider_id='alpha' AND model_id='shared'), \
                            (SELECT COUNT(*) FROM model_registry_heads \
                             WHERE provider_id='alpha' AND model_id='shared')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap()
}

async fn model_head(registry: &AgentRegistry) -> u64 {
    let revision = registry
        .run(|connection| {
            connection
                .query_row(
                    "SELECT active_revision FROM model_registry_heads \
                     WHERE provider_id='alpha' AND model_id='shared'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    u64::try_from(revision).unwrap()
}

#[tokio::test]
async fn create_and_stage_preflight_the_active_provider_before_mutation() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    install_providers(&registry).await;
    let unsupported = ProviderDefinition {
        id: "legacy".into(),
        revision: 1,
        kind: "unsupported".into(),
        base_url: "https://legacy.invalid".into(),
        credential_binding_id: "credential/main".into(),
    };
    registry.create_provider(unsupported).await.unwrap();
    let mut existing = model("legacy", 1, 100_000);
    existing.provider_id = "legacy".into();
    registry.seed_model(existing.clone()).await.unwrap();

    let mut created = model("legacy", 1, 100_000);
    created.provider_id = "legacy".into();
    created.model_id = "new".into();
    assert!(matches!(
        registry.create_model(created).await,
        Err(ModelRegistryError::IncompatibleProvider(_))
    ));
    assert!(
        registry
            .load_active_model(("legacy", "new"))
            .await
            .unwrap()
            .is_none()
    );

    existing.revision = 2;
    assert!(matches!(
        registry.stage_model(1, existing).await,
        Err(ModelRegistryError::IncompatibleProvider(_))
    ));
    assert_eq!(
        registry
            .inspect_model(("legacy", "shared"))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn model_head_changes_recheck_the_current_provider_before_mutation() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    install_providers(&registry).await;
    registry
        .create_model(model("alpha", 1, 100_000))
        .await
        .unwrap();
    registry
        .stage_model(1, model("alpha", 2, 120_000))
        .await
        .unwrap();
    let unsupported = ProviderDefinition {
        id: "alpha".into(),
        revision: 2,
        kind: "unsupported".into(),
        base_url: "https://alpha-v2.invalid".into(),
        credential_binding_id: "credential/main".into(),
    };
    registry.stage_provider(1, unsupported).await.unwrap();
    registry
        .run(|connection| {
            connection
                .execute(
                    "UPDATE provider_registry_heads SET active_revision=2 WHERE provider_id='alpha'",
                    [],
                )
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();

    assert!(matches!(
        registry.activate_model(("alpha", "shared"), 2, 1).await,
        Err(ModelRegistryError::IncompatibleProvider(_))
    ));
    assert_eq!(model_head(&registry).await, 1);
}

#[derive(Clone, Copy, Debug)]
enum ModelTamper {
    Json,
    Digest,
    ProviderIdentity,
    ModelIdentity,
    RevisionIdentity,
}

async fn tamper_model(registry: &AgentRegistry, revision: u64, tamper: ModelTamper) {
    let revision = i64::try_from(revision).unwrap();
    registry
        .run(move |connection| {
            if matches!(tamper, ModelTamper::Digest) {
                connection
                    .execute(
                        "UPDATE model_definitions SET digest='tampered' \
                         WHERE provider_id='alpha' AND model_id='shared' AND revision=?1",
                        [revision],
                    )
                    .map_err(AgentRegistryError::sqlite)?;
                return Ok(());
            }
            let json: String = connection
                .query_row(
                    "SELECT definition_json FROM model_definitions \
                     WHERE provider_id='alpha' AND model_id='shared' AND revision=?1",
                    [revision],
                    |row| row.get(0),
                )
                .map_err(AgentRegistryError::sqlite)?;
            if matches!(tamper, ModelTamper::Json) {
                connection
                    .execute(
                        "UPDATE model_definitions SET definition_json=?1 \
                         WHERE provider_id='alpha' AND model_id='shared' AND revision=?2",
                        params![format!(" {json}"), revision],
                    )
                    .map_err(AgentRegistryError::sqlite)?;
                return Ok(());
            }
            let mut definition: ModelDefinition =
                serde_json::from_str(&json).map_err(AgentRegistryError::serde)?;
            match tamper {
                ModelTamper::ProviderIdentity => definition.provider_id = "beta".into(),
                ModelTamper::ModelIdentity => definition.model_id = "other".into(),
                ModelTamper::RevisionIdentity => definition.revision += 1,
                ModelTamper::Json | ModelTamper::Digest => unreachable!(),
            }
            connection
                .execute(
                    "UPDATE model_definitions SET definition_json=?1 \
                     WHERE provider_id='alpha' AND model_id='shared' AND revision=?2",
                    params![
                        serde_json::to_string(&definition).map_err(AgentRegistryError::serde)?,
                        revision
                    ],
                )
                .map_err(AgentRegistryError::sqlite)?;
            Ok(())
        })
        .await
        .unwrap();
}

#[derive(Clone, Copy, Debug)]
enum ModelMutation {
    Stage,
    Activate,
    Rollback,
}

#[tokio::test]
async fn lifecycle_mutations_verify_complete_model_rows_before_moving_head() {
    for mutation in [
        ModelMutation::Stage,
        ModelMutation::Activate,
        ModelMutation::Rollback,
    ] {
        for tamper in [
            ModelTamper::Json,
            ModelTamper::Digest,
            ModelTamper::ProviderIdentity,
            ModelTamper::ModelIdentity,
            ModelTamper::RevisionIdentity,
        ] {
            let directory = tempdir().unwrap();
            let registry =
                AgentRegistry::open(directory.path().join(format!("{mutation:?}-{tamper:?}.db")))
                    .await
                    .unwrap();
            install_providers(&registry).await;
            registry.seed_model(model("alpha", 1, 100)).await.unwrap();
            registry
                .stage_model(1, model("alpha", 2, 200))
                .await
                .unwrap();
            let (target, expected) = match mutation {
                ModelMutation::Stage | ModelMutation::Activate => (2, 1),
                ModelMutation::Rollback => {
                    registry
                        .activate_model(("alpha", "shared"), 2, 1)
                        .await
                        .unwrap();
                    (1, 2)
                }
            };
            tamper_model(&registry, target, tamper).await;
            let before = model_head(&registry).await;
            let result = match mutation {
                ModelMutation::Stage => registry
                    .stage_model(expected, model("alpha", target, 200))
                    .await
                    .map(|_| ()),
                ModelMutation::Activate => registry
                    .activate_model(("alpha", "shared"), target, expected)
                    .await
                    .map(|_| ()),
                ModelMutation::Rollback => registry
                    .rollback_model(("alpha", "shared"), target, expected)
                    .await
                    .map(|_| ()),
            };
            assert!(result.is_err(), "{mutation:?} accepted {tamper:?}");
            assert_eq!(model_head(&registry).await, before);
        }
    }
}

#[tokio::test]
async fn strict_model_create_requires_revision_one_and_never_reuses_an_identity() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    install_providers(&registry).await;
    assert!(matches!(
        registry.create_model(model("alpha", 2, 200)).await,
        Err(ModelRegistryError::NonSequential {
            expected: 1,
            actual: 2,
            ..
        })
    ));

    let created = registry.create_model(model("alpha", 1, 100)).await.unwrap();
    assert!(created.active);
    assert_eq!(created.definition.revision, 1);
    for context_window in [100, 999] {
        assert!(matches!(
            registry
                .create_model(model("alpha", 1, context_window))
                .await,
            Err(ModelRegistryError::AlreadyExists { identity })
                if identity == "alpha/shared"
        ));
    }
    let stored = registry
        .load_active_model(("alpha", "shared"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.definition.context_window, 100);
    assert_eq!(model_row_counts(&registry).await, (1, 1));
}

#[tokio::test]
async fn strict_model_create_fails_closed_for_definition_only_or_head_only_state() {
    let directory = tempdir().unwrap();
    let definition_only = AgentRegistry::open(directory.path().join("definition-only.db"))
        .await
        .unwrap();
    install_providers(&definition_only).await;
    definition_only
        .seed_model(model("alpha", 1, 100))
        .await
        .unwrap();
    definition_only
        .run(|connection| {
            connection
                .execute("DELETE FROM model_registry_heads", [])
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        definition_only.create_model(model("alpha", 1, 999)).await,
        Err(ModelRegistryError::AlreadyExists { .. })
    ));
    assert_eq!(model_row_counts(&definition_only).await, (1, 0));

    let head_only = AgentRegistry::open(directory.path().join("head-only.db"))
        .await
        .unwrap();
    install_providers(&head_only).await;
    head_only.seed_model(model("alpha", 1, 100)).await.unwrap();
    head_only
        .run(|connection| {
            connection
                .execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM model_definitions;")
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        head_only.create_model(model("alpha", 1, 999)).await,
        Err(ModelRegistryError::AlreadyExists { .. })
    ));
    assert_eq!(model_row_counts(&head_only).await, (0, 1));
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
    let activated = registry
        .activate_model(("alpha", "shared"), 2, 1)
        .await
        .unwrap();
    assert!(activated.active);
    assert_eq!(activated.definition.revision, 2);
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
    let rolled_back = registry
        .rollback_model(("alpha", "shared"), 1, 2)
        .await
        .unwrap();
    assert!(rolled_back.active);
    assert_eq!(rolled_back.definition.revision, 1);
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
