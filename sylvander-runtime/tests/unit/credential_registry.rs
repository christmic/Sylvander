use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tempfile::tempdir;

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::config::{SecretRef, SecretResolver, SecretValue, SystemSecretResolver};
use crate::credential_registry::{CredentialRegistryError, CredentialSecretResolver};
use crate::registry_domain::CredentialBindingRevision;

fn credential(generation: u64, name: &str) -> CredentialBindingRevision {
    CredentialBindingRevision {
        binding_id: "credential/main".into(),
        generation,
        reference: SecretRef::Env { name: name.into() },
    }
}

fn file_credential(generation: u64, path: PathBuf) -> CredentialBindingRevision {
    CredentialBindingRevision {
        binding_id: "credential/live".into(),
        generation,
        reference: SecretRef::File { path },
    }
}

#[derive(Default)]
struct MockResolver {
    calls: AtomicUsize,
    fail: AtomicBool,
}

impl CredentialSecretResolver for MockResolver {
    fn resolve_credential(&self, reference: &SecretRef) -> Result<SecretValue, ()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(());
        }
        SystemSecretResolver.resolve(reference).map_err(|_| ())
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
async fn strict_create_is_idempotent_rejects_different_content_and_survives_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let registry = AgentRegistry::open(&path).await.unwrap();
    let reference = SecretRef::Env {
        name: "PROVIDER_KEY_ONE".into(),
    };
    let created = registry
        .create_credential_binding("credential/created", reference.clone())
        .await
        .unwrap();
    assert_eq!(created.definition.generation, 1);
    assert!(created.active);
    let retried = registry
        .create_credential_binding("credential/created", reference.clone())
        .await
        .unwrap();
    assert_eq!(retried, created);
    assert!(matches!(
        registry
            .create_credential_binding(
                "credential/created",
                SecretRef::Env {
                    name: "DIFFERENT_KEY".into()
                }
            )
            .await,
        Err(CredentialRegistryError::AlreadyExists { binding_id })
            if binding_id == "credential/created"
    ));
    drop(registry);

    let reopened = AgentRegistry::open(path).await.unwrap();
    let after_restart = reopened
        .create_credential_binding("credential/created", reference)
        .await
        .unwrap();
    assert_eq!(after_restart, created);
    assert_eq!(
        reopened
            .inspect_credentials("credential/created")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn strict_create_serializes_two_connections_for_same_and_different_references() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let first = AgentRegistry::open(&path).await.unwrap();
    let second = AgentRegistry::open(&path).await.unwrap();
    let same = SecretRef::Env {
        name: "SHARED_KEY".into(),
    };
    let same_results = tokio::join!(
        first.create_credential_binding("credential/same", same.clone()),
        second.create_credential_binding("credential/same", same)
    );
    let left = same_results.0.unwrap();
    let right = same_results.1.unwrap();
    assert_eq!(left, right);
    assert_eq!(left.definition.generation, 1);

    let different_results = tokio::join!(
        first.create_credential_binding(
            "credential/race",
            SecretRef::Env {
                name: "FIRST_KEY".into()
            }
        ),
        second.create_credential_binding(
            "credential/race",
            SecretRef::Env {
                name: "SECOND_KEY".into()
            }
        )
    );
    let outcomes = [different_results.0, different_results.1];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(
                result,
                Err(CredentialRegistryError::AlreadyExists { binding_id })
                    if binding_id == "credential/race"
            ))
            .count(),
        1
    );
    assert_eq!(
        first
            .inspect_credentials("credential/race")
            .await
            .unwrap()
            .len(),
        1
    );
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

#[tokio::test]
async fn request_scoped_resolution_reloads_rotates_and_fails_closed() {
    let directory = tempdir().unwrap();
    let first_path = directory.path().join("first.secret");
    let second_path = directory.path().join("second.secret");
    std::fs::write(&first_path, "first-value\n").unwrap();
    std::fs::write(&second_path, "second-value\n").unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    registry
        .seed_credential(file_credential(1, first_path.clone()))
        .await
        .unwrap();
    let resolver = MockResolver::default();

    let first = registry
        .resolve_active_credential("credential/live", &resolver)
        .await
        .unwrap();
    assert_eq!(first.generation(), 1);
    assert_eq!(first.value().as_str().unwrap(), "first-value");
    assert_eq!(format!("{first:?}"), "ResolvedCredential([REDACTED])");
    std::fs::write(&first_path, "first-value-updated\n").unwrap();
    let refreshed = registry
        .resolve_active_credential("credential/live", &resolver)
        .await
        .unwrap();
    assert_eq!(refreshed.value().as_str().unwrap(), "first-value-updated");
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);

    registry
        .stage_credential(1, file_credential(2, second_path))
        .await
        .unwrap();
    registry
        .activate_credential("credential/live", 2, 1)
        .await
        .unwrap();
    let rotated = registry
        .resolve_active_credential("credential/live", &resolver)
        .await
        .unwrap();
    assert_eq!(rotated.generation(), 2);
    assert_eq!(rotated.value().as_str().unwrap(), "second-value");

    resolver.fail.store(true, Ordering::SeqCst);
    let error = registry
        .resolve_active_credential("credential/live", &resolver)
        .await
        .unwrap_err();
    let displayed = error.to_string();
    assert!(matches!(
        error,
        CredentialRegistryError::Resolution { generation: 2, .. }
    ));
    assert_eq!(
        displayed,
        "credential `credential/live` generation 2 could not be resolved"
    );
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        registry
            .load_active_credential("credential/live")
            .await
            .unwrap()
            .unwrap()
            .definition
            .generation,
        2
    );
}

#[tokio::test]
async fn exact_generation_preflight_is_redacted_bounded_and_does_not_move_head() {
    let directory = tempdir().unwrap();
    let secret_path = directory.path().join("private-provider-secret");
    let invalid_path = directory.path().join("invalid-utf8-secret");
    std::fs::write(&secret_path, "available-secret\n").unwrap();
    std::fs::write(&invalid_path, [0xff, 0xfe]).unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let env_locator = "VERY_SECRET_ENV_LOCATOR";
    registry
        .seed_credential(CredentialBindingRevision {
            binding_id: "credential/live".into(),
            generation: 1,
            reference: SecretRef::Env {
                name: env_locator.into(),
            },
        })
        .await
        .unwrap();
    registry
        .stage_credential(1, file_credential(2, secret_path.clone()))
        .await
        .unwrap();
    registry
        .stage_credential(1, file_credential(3, invalid_path.clone()))
        .await
        .unwrap();
    let resolver = MockResolver::default();

    assert_eq!(
        registry
            .preflight_credential_generation("credential/live", 2, &resolver)
            .await
            .unwrap(),
        2
    );
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    let invalid = registry
        .preflight_credential_generation("credential/live", 3, &resolver)
        .await
        .unwrap_err();
    assert!(matches!(
        &invalid,
        CredentialRegistryError::Resolution { generation: 3, .. }
    ));
    let invalid_rendered = format!("{invalid:?} {invalid}");
    assert!(!invalid_rendered.contains(invalid_path.to_string_lossy().as_ref()));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
    resolver.fail.store(true, Ordering::SeqCst);
    let failed = registry
        .preflight_credential_generation("credential/live", 1, &resolver)
        .await
        .unwrap_err();
    assert!(matches!(
        &failed,
        CredentialRegistryError::Resolution { generation: 1, .. }
    ));
    let rendered = format!("{failed:?} {failed}");
    assert!(!rendered.contains(env_locator));
    assert!(!rendered.contains(secret_path.to_string_lossy().as_ref()));
    assert!(!rendered.contains(invalid_path.to_string_lossy().as_ref()));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 3);

    let unknown = registry
        .preflight_credential_generation("credential/live", 99, &resolver)
        .await
        .unwrap_err();
    assert!(matches!(
        unknown,
        CredentialRegistryError::UnknownGeneration { generation: 99, .. }
    ));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        registry
            .load_active_credential("credential/live")
            .await
            .unwrap()
            .unwrap()
            .definition
            .generation,
        1
    );
}

#[tokio::test]
async fn bounded_pages_are_descending_exclusive_and_restart_safe() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let registry = AgentRegistry::open(&path).await.unwrap();
    registry
        .seed_credential(credential(1, "PROVIDER_KEY_ONE"))
        .await
        .unwrap();
    for generation in 2..=5 {
        registry
            .stage_credential(
                1,
                credential(generation, &format!("PROVIDER_KEY_{generation}")),
            )
            .await
            .unwrap();
    }
    registry
        .activate_credential("credential/main", 4, 1)
        .await
        .unwrap();

    let first = registry
        .inspect_credential_page("credential/main", None, 2)
        .await
        .unwrap();
    assert_eq!(first.active_generation, 4);
    assert_eq!(
        first
            .generations
            .iter()
            .map(|view| view.generation)
            .collect::<Vec<_>>(),
        [5, 4]
    );
    assert_eq!(first.next_before_generation, Some(4));
    assert!(first.generations[1].active);
    drop(registry);

    let reopened = AgentRegistry::open(path).await.unwrap();
    let second = reopened
        .inspect_credential_page("credential/main", first.next_before_generation, 2)
        .await
        .unwrap();
    assert_eq!(second.active_generation, 4);
    assert_eq!(
        second
            .generations
            .iter()
            .map(|view| view.generation)
            .collect::<Vec<_>>(),
        [3, 2]
    );
    assert_eq!(second.next_before_generation, Some(2));
    let final_page = reopened
        .inspect_credential_page("credential/main", Some(2), 2)
        .await
        .unwrap();
    assert_eq!(
        final_page
            .generations
            .iter()
            .map(|view| view.generation)
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(final_page.next_before_generation, None);
}

#[tokio::test]
async fn bounded_page_distinguishes_unknown_binding_and_integrity_failures() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    assert!(matches!(
        registry
            .inspect_credential_page("credential/unknown", None, 10)
            .await,
        Err(CredentialRegistryError::UnknownBinding(binding))
            if binding == "credential/unknown"
    ));
    registry
        .seed_credential(credential(1, "PROVIDER_KEY_ONE"))
        .await
        .unwrap();
    registry
        .run(|connection| {
            connection
                .execute("DELETE FROM credential_binding_heads", [])
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        registry
            .inspect_credential_page("credential/main", None, 10)
            .await,
        Err(CredentialRegistryError::Registry(
            AgentRegistryError::Integrity(_)
        ))
    ));

    let registry = AgentRegistry::open(directory.path().join("tampered.db"))
        .await
        .unwrap();
    registry
        .seed_credential(credential(1, "PROVIDER_KEY_ONE"))
        .await
        .unwrap();
    registry
        .run(|connection| {
            connection
                .execute(
                    "UPDATE credential_binding_revisions SET digest='tampered'",
                    [],
                )
                .map(|_| ())
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert!(matches!(
        registry
            .inspect_credential_page("credential/main", None, 10)
            .await,
        Err(CredentialRegistryError::Registry(
            AgentRegistryError::Integrity(_)
        ))
    ));
}
