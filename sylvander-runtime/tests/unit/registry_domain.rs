use std::collections::BTreeSet;
use std::path::PathBuf;

use rusqlite::params;
use sylvander_protocol::ModelLifecycle;
use tempfile::tempdir;

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::config::SecretRef;
use crate::registry_domain::{
    CredentialBindingRevision, ModelDefinition, ProviderDefinition, canonical_definition,
    canonical_secret_reference,
};

fn provider(id: &str, revision: u64) -> ProviderDefinition {
    ProviderDefinition {
        id: id.into(),
        revision,
        kind: "anthropic_compatible".into(),
        base_url: format!("https://{id}.invalid"),
        credential_binding_id: "credential/main".into(),
    }
}

fn model(provider_id: &str) -> ModelDefinition {
    ModelDefinition {
        provider_id: provider_id.into(),
        model_id: "shared".into(),
        revision: 1,
        context_window: 100,
        max_output_tokens: 10,
        capabilities: BTreeSet::from(["tool_use".into()]),
        lifecycle: ModelLifecycle::Active,
        pricing: None,
    }
}

async fn insert_fixture(registry: &AgentRegistry) {
    let credential = CredentialBindingRevision {
        binding_id: "credential/main".into(),
        generation: 1,
        reference: SecretRef::File {
            path: PathBuf::from("/private/provider.key"),
        },
    };
    let credential = canonical_secret_reference(&credential.reference).unwrap();
    let providers = [
        provider("alpha", 1),
        provider("alpha", 2),
        provider("beta", 1),
        provider("staged", 1),
    ]
    .map(|value| (value.clone(), canonical_definition(&value).unwrap()));
    let models = [model("alpha"), model("beta")]
        .map(|value| (value.clone(), canonical_definition(&value).unwrap()));
    registry
        .run(move |connection| {
            connection
                .execute(
                    "INSERT INTO credential_binding_revisions VALUES ('credential/main',1,?1,?2,1)",
                    params![credential.0, credential.1],
                )
                .map_err(AgentRegistryError::sqlite)?;
            connection
                .execute(
                    "INSERT INTO credential_binding_heads VALUES ('credential/main',1,1)",
                    [],
                )
                .map_err(AgentRegistryError::sqlite)?;
            for (definition, encoded) in providers {
                connection
                    .execute(
                        "INSERT INTO provider_definitions VALUES (?1,?2,?3,?4,'credential/main',1)",
                        params![
                            definition.id,
                            i64::try_from(definition.revision).unwrap(),
                            encoded.0,
                            encoded.1
                        ],
                    )
                    .map_err(AgentRegistryError::sqlite)?;
            }
            for id in ["alpha", "beta"] {
                connection
                    .execute("INSERT INTO provider_registry_heads VALUES (?1,1,1)", [id])
                    .map_err(AgentRegistryError::sqlite)?;
            }
            for (definition, encoded) in models {
                connection
                    .execute(
                        "INSERT INTO model_definitions VALUES (?1,'shared',1,?2,?3,1)",
                        params![definition.provider_id, encoded.0, encoded.1],
                    )
                    .map_err(AgentRegistryError::sqlite)?;
                connection
                    .execute(
                        "INSERT INTO model_registry_heads VALUES (?1,'shared',1,1)",
                        [definition.provider_id],
                    )
                    .map_err(AgentRegistryError::sqlite)?;
            }
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn file_backed_reads_preserve_qualified_identity_and_staged_state() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let registry = AgentRegistry::open(&path).await.unwrap();
    insert_fixture(&registry).await;
    drop(registry);
    let registry = AgentRegistry::open(path).await.unwrap();

    assert!(
        !registry
            .load_provider_revision("staged", 1)
            .await
            .unwrap()
            .unwrap()
            .active
    );
    assert!(
        !registry
            .load_provider_revision("alpha", 2)
            .await
            .unwrap()
            .unwrap()
            .active
    );
    for id in ["alpha", "beta"] {
        let stored = registry
            .load_model_revision(id, "shared", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.definition.provider_id, id);
        assert!(stored.active && !stored.digest.is_empty());
    }
}

#[tokio::test]
async fn redaction_and_integrity_checks_reject_tampered_rows() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    insert_fixture(&registry).await;
    let view = registry
        .inspect_credential_revision("credential/main", 1)
        .await
        .unwrap()
        .unwrap();
    let inspected = serde_json::to_string(&view).unwrap();
    assert!(view.active && view.reference_configured);
    assert!(!inspected.contains("/private/provider.key"));

    registry.run(|connection| {
        connection.execute("UPDATE provider_definitions SET digest='tampered' WHERE provider_id='alpha' AND revision=1", []).map_err(AgentRegistryError::sqlite)?;
        connection.execute("UPDATE credential_binding_revisions SET digest='tampered'", []).map_err(AgentRegistryError::sqlite)?;
        let wrong = model("wrong");
        let encoded = canonical_definition(&wrong)?;
        connection.execute("UPDATE model_definitions SET definition_json=?1,digest=?2 WHERE provider_id='beta' AND model_id='shared'", params![encoded.0, encoded.1]).map_err(AgentRegistryError::sqlite)?;
        Ok(())
    }).await.unwrap();
    assert!(matches!(
        registry.load_provider_revision("alpha", 1).await,
        Err(AgentRegistryError::Integrity(_))
    ));
    assert!(matches!(
        registry.load_model_revision("beta", "shared", 1).await,
        Err(AgentRegistryError::Integrity(_))
    ));
    assert!(matches!(
        registry
            .load_credential_revision("credential/main", 1)
            .await,
        Err(AgentRegistryError::Integrity(_))
    ));
}
