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
