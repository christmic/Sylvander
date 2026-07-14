use tempfile::tempdir;

use super::{
    AgentRegistry, AgentRegistryError, REGISTRY_COMPONENT, REGISTRY_SCHEMA_VERSION, SCHEMA,
    hex_digest,
};
use crate::config::ServerConfig;

fn catalog() -> ServerConfig {
    ServerConfig::from_toml(include_str!("../../../config/sylvander.example.toml")).unwrap()
}

#[tokio::test]
async fn legacy_file_migrates_once_and_enforces_registry_foreign_keys() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("legacy.db");
    let legacy = rusqlite::Connection::open(&path).unwrap();
    legacy.execute_batch(SCHEMA).unwrap();
    drop(legacy);

    let registry = AgentRegistry::open(&path).await.unwrap();
    let state = registry
        .run(|connection| {
            let foreign_keys: i64 = connection
                .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                .map_err(AgentRegistryError::sqlite)?;
            let busy_timeout: i64 = connection
                .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
                .map_err(AgentRegistryError::sqlite)?;
            let version: i64 = connection
                .query_row(
                    "SELECT MAX(version) FROM schema_migrations WHERE component=?1",
                    [REGISTRY_COMPONENT],
                    |row| row.get(0),
                )
                .map_err(AgentRegistryError::sqlite)?;
            let tables: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN (\
                     'credential_binding_revisions','credential_binding_heads',\
                     'provider_definitions','provider_registry_heads',\
                     'model_definitions','model_registry_heads')",
                    [],
                    |row| row.get(0),
                )
                .map_err(AgentRegistryError::sqlite)?;
            Ok((foreign_keys, busy_timeout, version, tables))
        })
        .await
        .unwrap();
    assert_eq!(state, (1, 5_000, REGISTRY_SCHEMA_VERSION, 6));

    let foreign_key_error = registry
        .run(|connection| {
            connection
                .execute(
                    "INSERT INTO provider_registry_heads(provider_id,active_revision,updated_at) \
                     VALUES ('missing',1,0)",
                    [],
                )
                .map_err(AgentRegistryError::sqlite)?;
            Ok(())
        })
        .await;
    assert!(matches!(
        foreign_key_error,
        Err(AgentRegistryError::Storage(_))
    ));
    drop(registry);

    let reopened = AgentRegistry::open(path).await.unwrap();
    let migration_rows: i64 = reopened
        .run(|connection| {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM schema_migrations WHERE component=?1",
                    [REGISTRY_COMPONENT],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(AgentRegistryError::sqlite)
        })
        .await
        .unwrap();
    assert_eq!(migration_rows, 1);
}

#[tokio::test]
async fn revision_lifecycle_survives_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let catalog = catalog();
    let original = catalog.agents[0].clone();
    let agent_id = original.spec.id.clone();
    let registry = AgentRegistry::open(&path).await.unwrap();

    registry.seed(&catalog).await.unwrap();
    let active = registry.load_active(&agent_id).await.unwrap().unwrap();
    assert_eq!(active.definition.revision, original.revision);

    let mut next = original.clone();
    next.revision += 1;
    next.spec.name = "Revised Agent".into();
    let staged = registry
        .update(&catalog, original.revision, next.clone())
        .await
        .unwrap();
    assert!(!staged.active);
    assert_eq!(
        registry
            .load_active(&agent_id)
            .await
            .unwrap()
            .unwrap()
            .definition
            .revision,
        original.revision
    );

    registry
        .activate(&agent_id, next.revision, original.revision)
        .await
        .unwrap();
    drop(registry);

    let reopened = AgentRegistry::open(path).await.unwrap();
    assert_eq!(
        reopened
            .load_active(&agent_id)
            .await
            .unwrap()
            .unwrap()
            .definition
            .spec
            .name,
        "Revised Agent"
    );
    assert!(
        reopened
            .load(&agent_id, original.revision)
            .await
            .unwrap()
            .is_some()
    );
    reopened
        .rollback(&agent_id, original.revision, next.revision)
        .await
        .unwrap();
    let revisions = reopened.inspect(&agent_id).await.unwrap();
    assert_eq!(revisions.len(), 2);
    assert!(
        revisions
            .iter()
            .any(|revision| revision.active && revision.definition.revision == original.revision)
    );
}

#[tokio::test]
async fn update_validates_catalog_and_uses_optimistic_concurrency() {
    let catalog = catalog();
    let original = catalog.agents[0].clone();
    let registry = AgentRegistry::open_in_memory().await.unwrap();
    registry.seed(&catalog).await.unwrap();

    let mut invalid = original.clone();
    invalid.revision += 1;
    invalid.spec.model.model_name = "missing-model".into();
    assert!(matches!(
        registry.update(&catalog, original.revision, invalid).await,
        Err(AgentRegistryError::Invalid(_))
    ));

    let mut next = original.clone();
    next.revision += 1;
    registry
        .update(&catalog, original.revision, next.clone())
        .await
        .unwrap();
    assert!(matches!(
        registry
            .activate(&original.spec.id, next.revision, original.revision + 9)
            .await,
        Err(AgentRegistryError::Conflict { .. })
    ));
}

#[tokio::test]
async fn immutable_revision_rejects_changed_content() {
    let mut catalog = catalog();
    let registry = AgentRegistry::open_in_memory().await.unwrap();
    registry.seed(&catalog).await.unwrap();
    catalog.agents[0].spec.name = "Conflicting content".into();

    assert!(matches!(
        registry.seed(&catalog).await,
        Err(AgentRegistryError::RevisionCollision { .. })
    ));
}

#[tokio::test]
async fn load_rejects_tampered_digest_and_definition_identity() {
    let catalog = catalog();
    let original = catalog.agents[0].clone();
    let agent_id = original.spec.id.clone();
    let registry = AgentRegistry::open_in_memory().await.unwrap();
    registry.seed(&catalog).await.unwrap();

    let id = agent_id.0.clone();
    registry
        .run(move |connection| {
            connection
                .execute(
                    "UPDATE agent_definitions SET digest='tampered' WHERE agent_id=?1 AND revision=1",
                    [id],
                )
                .map_err(AgentRegistryError::sqlite)?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        registry.load(&agent_id, 1).await,
        Err(AgentRegistryError::Integrity(_))
    ));

    let mut mismatched = original;
    mismatched.spec.id = sylvander_protocol::AgentId::new("other-agent");
    let json = serde_json::to_string(&mismatched).unwrap();
    let digest = hex_digest(json.as_bytes());
    let id = agent_id.0.clone();
    registry
        .run(move |connection| {
            connection
                .execute(
                    "UPDATE agent_definitions SET definition_json=?2, digest=?3 \
                     WHERE agent_id=?1 AND revision=1",
                    rusqlite::params![id, json, digest],
                )
                .map_err(AgentRegistryError::sqlite)?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        registry.inspect(&agent_id).await,
        Err(AgentRegistryError::Integrity(_))
    ));
}
