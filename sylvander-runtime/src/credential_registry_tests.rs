use std::path::Path;

use tempfile::tempdir;

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::config::SecretRef;
use crate::credential_registry::CredentialRegistryError;
use crate::registry_domain::CredentialBindingRevision;

fn credential(generation: u64, name: &str) -> CredentialBindingRevision {
    CredentialBindingRevision {
        binding_id: "credential/main".into(),
        generation,
        reference: SecretRef::Env { name: name.into() },
    }
}

async fn open_pair(path: &Path) -> (AgentRegistry, AgentRegistry) {
    let first = AgentRegistry::open(path).await.unwrap();
    first
        .seed_credential(credential(1, "PROVIDER_KEY_ONE"))
        .await
        .unwrap();
    let second = AgentRegistry::open(path).await.unwrap();
    (first, second)
}

#[tokio::test]
async fn lifecycle_is_immutable_redacted_and_survives_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let registry = AgentRegistry::open(&path).await.unwrap();
    let seeded = registry
        .seed_credential(credential(1, "PROVIDER_KEY_ONE"))
        .await
        .unwrap();
    assert!(seeded.active);
    let existing = registry
        .seed_credential(credential(1, "MUST_NOT_REPLACE_ACTIVE"))
        .await
        .unwrap();
    assert_eq!(existing.definition.reference, seeded.definition.reference);

    let staged = registry
        .stage_credential(1, credential(2, "PROVIDER_KEY_TWO"))
        .await
        .unwrap();
    assert!(!staged.active);
    assert_eq!(
        registry
            .stage_credential(1, credential(2, "PROVIDER_KEY_TWO"))
            .await
            .unwrap()
            .digest,
        staged.digest
    );
    assert!(matches!(
        registry
            .stage_credential(1, credential(2, "DIFFERENT_KEY"))
            .await,
        Err(CredentialRegistryError::GenerationCollision { generation: 2, .. })
    ));
    assert!(matches!(
        registry
            .stage_credential(1, credential(4, "SKIPPED_KEY"))
            .await,
        Err(CredentialRegistryError::NonSequential { expected: 3, .. })
    ));

    registry
        .activate_credential("credential/main", 2, 1)
        .await
        .unwrap();
    drop(registry);
    let registry = AgentRegistry::open(&path).await.unwrap();
    assert_eq!(
        registry
            .load_active_credential("credential/main")
            .await
            .unwrap()
            .unwrap()
            .definition
            .generation,
        2
    );
    let views = registry
        .inspect_credentials("credential/main")
        .await
        .unwrap();
    let encoded = serde_json::to_string(&views).unwrap();
    assert_eq!(views.len(), 2);
    assert!(views[0].active && !views[1].active);
    for hidden in ["PROVIDER_KEY_ONE", "PROVIDER_KEY_TWO", "path", "name"] {
        assert!(!encoded.contains(hidden), "inspect leaked {hidden}");
    }

    registry
        .rollback_credential("credential/main", 1, 2)
        .await
        .unwrap();
    assert_eq!(
        registry
            .load_active_credential("credential/main")
            .await
            .unwrap()
            .unwrap()
            .definition
            .generation,
        1
    );
}

#[tokio::test]
async fn two_file_connections_enforce_expected_head_cas() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let (first, second) = open_pair(&path).await;
    first
        .stage_credential(1, credential(2, "PROVIDER_KEY_TWO"))
        .await
        .unwrap();
    assert!(matches!(
        first.activate_credential("credential/main", 99, 1).await,
        Err(CredentialRegistryError::UnknownGeneration { generation: 99, .. })
    ));
    assert_eq!(
        first
            .load_active_credential("credential/main")
            .await
            .unwrap()
            .unwrap()
            .definition
            .generation,
        1
    );

    let results = tokio::join!(
        first.activate_credential("credential/main", 2, 1),
        second.activate_credential("credential/main", 2, 1)
    );
    let values = [results.0, results.1];
    assert_eq!(values.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        values
            .iter()
            .filter(|result| matches!(
                result,
                Err(CredentialRegistryError::Conflict {
                    expected: 1,
                    actual: 2,
                    ..
                })
            ))
            .count(),
        1
    );
    assert_eq!(
        first
            .load_active_credential("credential/main")
            .await
            .unwrap()
            .unwrap()
            .definition
            .generation,
        2
    );

    let rollback = tokio::join!(
        first.rollback_credential("credential/main", 1, 2),
        second.rollback_credential("credential/main", 1, 2)
    );
    let values = [rollback.0, rollback.1];
    assert_eq!(values.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        values
            .iter()
            .filter(|result| matches!(
                result,
                Err(CredentialRegistryError::Conflict {
                    expected: 2,
                    actual: 1,
                    ..
                })
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn database_and_inspection_never_contain_resolved_values_or_file_paths() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let path = "/private/credentials/provider-secret";
    let definition = CredentialBindingRevision {
        binding_id: "credential/file".into(),
        generation: 1,
        reference: SecretRef::File { path: path.into() },
    };
    assert!(!format!("{definition:?}").contains(path));
    registry.seed_credential(definition).await.unwrap();

    let views = registry
        .inspect_credentials("credential/file")
        .await
        .unwrap();
    let encoded = serde_json::to_string(&views).unwrap();
    assert!(!encoded.contains(path));
    assert!(!encoded.contains("provider-secret"));

    let known_resolved_value = "SUPER_SECRET_VALUE_MUST_NEVER_PERSIST".to_string();
    let occurrences = registry
        .run(move |connection| {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM credential_binding_revisions \
                     WHERE instr(reference_json,?1)>0 OR instr(digest,?1)>0",
                    [&known_resolved_value],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert_eq!(occurrences, 0);
}
