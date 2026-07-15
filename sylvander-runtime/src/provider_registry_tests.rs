use rusqlite::params;
use tempfile::tempdir;

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::config::SecretRef;
use crate::provider_registry::ProviderRegistryError;
use crate::registry_domain::{ProviderDefinition, canonical_secret_reference};

fn provider(revision: u64, base_url: &str) -> ProviderDefinition {
    ProviderDefinition {
        id: "provider/main".into(),
        revision,
        kind: "anthropic_compatible".into(),
        base_url: base_url.into(),
        credential_binding_id: "credential/main".into(),
    }
}

async fn install_credential(registry: &AgentRegistry) {
    let (reference, digest) = canonical_secret_reference(&SecretRef::Env {
        name: "PROVIDER_API_KEY".into(),
    })
    .unwrap();
    registry
        .run(move |connection| {
            connection
                .execute(
                    "INSERT INTO credential_binding_revisions VALUES (?1,1,?2,?3,1)",
                    params!["credential/main", reference, digest],
                )
                .map_err(AgentRegistryError::sqlite)?;
            connection
                .execute(
                    "INSERT INTO credential_binding_heads VALUES (?1,1,1)",
                    ["credential/main"],
                )
                .map_err(AgentRegistryError::sqlite)?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn open_pair(path: &std::path::Path) -> (AgentRegistry, AgentRegistry) {
    let first = AgentRegistry::open(path).await.unwrap();
    install_credential(&first).await;
    let second = AgentRegistry::open(path).await.unwrap();
    (first, second)
}

async fn provider_row_counts(registry: &AgentRegistry) -> (i64, i64) {
    registry
        .run(|connection| {
            connection
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM provider_definitions), \
                            (SELECT COUNT(*) FROM provider_registry_heads)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn strict_create_requires_revision_one_and_never_reuses_an_identity() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    install_credential(&registry).await;
    assert!(matches!(
        registry
            .create_provider(provider(2, "https://two.invalid"))
            .await,
        Err(ProviderRegistryError::NonSequential {
            expected: 1,
            actual: 2,
            ..
        })
    ));

    let created = registry
        .create_provider(provider(1, "https://one.invalid"))
        .await
        .unwrap();
    assert!(created.active);
    assert_eq!(created.definition.revision, 1);
    for duplicate in ["https://one.invalid", "https://different.invalid"] {
        assert!(matches!(
            registry.create_provider(provider(1, duplicate)).await,
            Err(ProviderRegistryError::AlreadyExists { provider_id })
                if provider_id == "provider/main"
        ));
    }
    let stored = registry
        .load_active_provider("provider/main")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.definition.base_url, "https://one.invalid");
    assert_eq!(provider_row_counts(&registry).await, (1, 1));
}

#[tokio::test]
async fn strict_create_fails_closed_for_definition_only_or_head_only_state() {
    let directory = tempdir().unwrap();
    let definition_only = AgentRegistry::open(directory.path().join("definition-only.db"))
        .await
        .unwrap();
    install_credential(&definition_only).await;
    definition_only
        .seed_provider(provider(1, "https://original.invalid"))
        .await
        .unwrap();
    definition_only
        .run(|connection| {
            connection
                .execute("DELETE FROM provider_registry_heads", [])
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        definition_only
            .create_provider(provider(1, "https://replacement.invalid"))
            .await,
        Err(ProviderRegistryError::AlreadyExists { .. })
    ));
    assert_eq!(provider_row_counts(&definition_only).await, (1, 0));

    let head_only = AgentRegistry::open(directory.path().join("head-only.db"))
        .await
        .unwrap();
    install_credential(&head_only).await;
    head_only
        .seed_provider(provider(1, "https://original.invalid"))
        .await
        .unwrap();
    head_only
        .run(|connection| {
            connection
                .execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM provider_definitions;")
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        head_only
            .create_provider(provider(1, "https://replacement.invalid"))
            .await,
        Err(ProviderRegistryError::AlreadyExists { .. })
    ));
    assert_eq!(provider_row_counts(&head_only).await, (0, 1));
}

fn assert_one_conflict(
    results: (
        Result<(), ProviderRegistryError>,
        Result<(), ProviderRegistryError>,
    ),
) {
    let values = [results.0, results.1];
    assert_eq!(values.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        values
            .iter()
            .filter(|result| matches!(result, Err(ProviderRegistryError::Conflict { .. })))
            .count(),
        1
    );
}

#[tokio::test]
async fn provider_lifecycle_is_immutable_and_survives_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let registry = AgentRegistry::open(&path).await.unwrap();
    install_credential(&registry).await;

    let seeded = registry
        .seed_provider(provider(1, "https://one.invalid"))
        .await
        .unwrap();
    assert!(seeded.active);
    assert_eq!(
        registry
            .seed_provider(provider(1, "https://ignored.invalid"))
            .await
            .unwrap()
            .definition,
        seeded.definition
    );

    let staged = registry
        .stage_provider(1, provider(2, "https://two.invalid"))
        .await
        .unwrap();
    assert!(!staged.active);
    assert_eq!(
        registry
            .stage_provider(1, provider(2, "https://two.invalid"))
            .await
            .unwrap()
            .digest,
        staged.digest
    );
    assert!(matches!(
        registry
            .stage_provider(1, provider(2, "https://collision.invalid"))
            .await,
        Err(ProviderRegistryError::RevisionCollision { revision: 2, .. })
    ));
    registry
        .stage_provider(1, provider(3, "https://three.invalid"))
        .await
        .unwrap();
    registry
        .activate_provider("provider/main", 2, 1)
        .await
        .unwrap();
    assert!(matches!(
        registry.activate_provider("provider/main", 3, 1).await,
        Err(ProviderRegistryError::Conflict {
            expected: 1,
            actual: 2,
            ..
        })
    ));
    assert_eq!(
        registry
            .load_active_provider("provider/main")
            .await
            .unwrap()
            .unwrap()
            .definition
            .revision,
        2
    );

    drop(registry);
    let registry = AgentRegistry::open(path).await.unwrap();
    let revisions = registry.inspect_provider("provider/main").await.unwrap();
    assert_eq!(
        revisions
            .iter()
            .map(|item| item.definition.revision)
            .collect::<Vec<_>>(),
        [3, 2, 1]
    );
    assert_eq!(revisions.iter().filter(|item| item.active).count(), 1);
    assert!(
        revisions
            .iter()
            .find(|item| item.active)
            .unwrap()
            .definition
            .revision
            == 2
    );

    registry
        .rollback_provider("provider/main", 1, 2)
        .await
        .unwrap();
    assert!(matches!(
        registry.activate_provider("provider/main", 99, 1).await,
        Err(ProviderRegistryError::UnknownRevision { revision: 99, .. })
    ));
    assert_eq!(
        registry
            .load_active_provider("provider/main")
            .await
            .unwrap()
            .unwrap()
            .definition
            .revision,
        1
    );
}

#[tokio::test]
async fn two_connections_allow_only_one_activation_and_rollback() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let (first, second) = open_pair(&path).await;
    first
        .seed_provider(provider(1, "https://one.invalid"))
        .await
        .unwrap();
    first
        .stage_provider(1, provider(2, "https://two.invalid"))
        .await
        .unwrap();
    first
        .stage_provider(1, provider(3, "https://three.invalid"))
        .await
        .unwrap();

    assert_one_conflict(tokio::join!(
        first.activate_provider("provider/main", 2, 1),
        second.activate_provider("provider/main", 3, 1)
    ));
    let active = first
        .load_active_provider("provider/main")
        .await
        .unwrap()
        .unwrap()
        .definition
        .revision;
    if active == 2 {
        first
            .activate_provider("provider/main", 3, 2)
            .await
            .unwrap();
    }

    assert_one_conflict(tokio::join!(
        first.rollback_provider("provider/main", 1, 3),
        second.rollback_provider("provider/main", 2, 3)
    ));
    assert!(matches!(
        first.rollback_provider("provider/main", 1, 3).await,
        Err(ProviderRegistryError::Conflict { expected: 3, .. })
    ));
}

#[tokio::test]
async fn bounded_provider_pages_are_exclusive_and_survive_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let registry = AgentRegistry::open(&path).await.unwrap();
    install_credential(&registry).await;
    registry
        .seed_provider(provider(1, "https://one.invalid"))
        .await
        .unwrap();
    for revision in 2..=5 {
        registry
            .stage_provider(
                1,
                provider(revision, &format!("https://{revision}.invalid")),
            )
            .await
            .unwrap();
    }
    registry
        .activate_provider("provider/main", 4, 1)
        .await
        .unwrap();

    let first = registry
        .inspect_provider_page("provider/main", None, 2)
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
    assert_eq!(first.next_before_revision, Some(4));
    assert!(first.revisions[1].active);
    drop(registry);

    let reopened = AgentRegistry::open(path).await.unwrap();
    let second = reopened
        .inspect_provider_page("provider/main", first.next_before_revision, 2)
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
        .inspect_provider_page("provider/main", Some(2), 2)
        .await
        .unwrap();
    assert_eq!(
        final_page
            .revisions
            .iter()
            .map(|stored| stored.definition.revision)
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(final_page.next_before_revision, None);
}

#[tokio::test]
async fn bounded_provider_page_distinguishes_unknown_and_integrity_failures() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    install_credential(&registry).await;
    assert!(matches!(
        registry
            .inspect_provider_page("provider/unknown", None, 10)
            .await,
        Err(ProviderRegistryError::UnknownProvider(provider))
            if provider == "provider/unknown"
    ));
    registry
        .seed_provider(provider(1, "https://one.invalid"))
        .await
        .unwrap();
    registry
        .run(|connection| {
            connection
                .execute("DELETE FROM provider_registry_heads", [])
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        registry
            .inspect_provider_page("provider/main", None, 10)
            .await,
        Err(ProviderRegistryError::Registry(
            AgentRegistryError::Integrity(_)
        ))
    ));

    let registry = AgentRegistry::open(directory.path().join("tampered.db"))
        .await
        .unwrap();
    install_credential(&registry).await;
    registry
        .seed_provider(provider(1, "https://one.invalid"))
        .await
        .unwrap();
    registry
        .run(|connection| {
            connection
                .execute("UPDATE provider_definitions SET digest='tampered'", [])
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        registry
            .inspect_provider_page("provider/main", None, 10)
            .await,
        Err(ProviderRegistryError::Registry(
            AgentRegistryError::Integrity(_)
        ))
    ));
}
