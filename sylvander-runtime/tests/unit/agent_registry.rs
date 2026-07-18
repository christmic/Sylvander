use tempfile::tempdir;

use sylvander_agent::session_store::{SESSION_SCHEMA_OBJECT_NAMES, SqliteSessionStore};

use super::{
    AgentRegistry, AgentRegistryError, REGISTRY_COMPONENT, REGISTRY_SCHEMA_OBJECT_NAMES,
    REGISTRY_SCHEMA_VERSION, hex_digest,
};
use crate::config::ServerConfig;

fn catalog() -> ServerConfig {
    ServerConfig::from_toml(include_str!("../../../config/sylvander.example.toml")).unwrap()
}

#[tokio::test]
async fn new_registry_creates_only_the_current_ledger_and_reopens_exactly() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let registry = AgentRegistry::open(&path).await.unwrap();
    let state = registry
        .run(|connection| {
            let foreign_keys: i64 = connection
                .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                .map_err(AgentRegistryError::sqlite)?;
            let busy_timeout: i64 = connection
                .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
                .map_err(AgentRegistryError::sqlite)?;
            let ledger: Vec<(String, i64)> = {
                let mut statement = connection
                    .prepare(
                        "SELECT component,version FROM schema_migrations \
                         ORDER BY component,version",
                    )
                    .map_err(AgentRegistryError::sqlite)?;
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                    .map_err(AgentRegistryError::sqlite)?
                    .collect::<Result<_, _>>()
                    .map_err(AgentRegistryError::sqlite)?
            };
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
            let snapshot_tables: (i64, i64) = connection
                .query_row(
                    "SELECT \
                       SUM(name IN (
                         'agent_registry_snapshots_v3',
                         'agent_registry_snapshot_providers_v3',
                         'agent_registry_snapshot_models_v3'
                       )), \
                       SUM(name IN (
                         'agent_registry_snapshots',
                         'agent_registry_snapshot_models'
                       )) \
                     FROM sqlite_schema WHERE type='table'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(AgentRegistryError::sqlite)?;
            Ok((foreign_keys, busy_timeout, ledger, tables, snapshot_tables))
        })
        .await
        .unwrap();
    assert_eq!(
        state,
        (
            1,
            5_000,
            vec![(REGISTRY_COMPONENT.into(), REGISTRY_SCHEMA_VERSION)],
            6,
            (3, 0)
        )
    );

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

    AgentRegistry::open(path).await.unwrap();
}

#[tokio::test]
async fn shared_session_and_registry_schema_survive_fresh_boot_and_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("shared.db");

    drop(
        SqliteSessionStore::open_shared(&path, REGISTRY_SCHEMA_OBJECT_NAMES)
            .await
            .unwrap(),
    );
    drop(
        AgentRegistry::open_shared(&path, SESSION_SCHEMA_OBJECT_NAMES)
            .await
            .unwrap(),
    );

    drop(
        SqliteSessionStore::open_shared(&path, REGISTRY_SCHEMA_OBJECT_NAMES)
            .await
            .unwrap(),
    );
    drop(
        AgentRegistry::open_shared(&path, SESSION_SCHEMA_OBJECT_NAMES)
            .await
            .unwrap(),
    );

    assert!(matches!(
        AgentRegistry::open(&path).await,
        Err(AgentRegistryError::Integrity(_))
    ));
}

#[tokio::test]
async fn shared_registry_rejects_unknown_and_partial_registry_namespaces() {
    let directory = tempdir().unwrap();
    let unknown_path = directory.path().join("unknown.db");
    drop(SqliteSessionStore::open(&unknown_path).await.unwrap());
    let connection = rusqlite::Connection::open(&unknown_path).unwrap();
    connection
        .execute_batch("CREATE TABLE undeclared_runtime_state(value TEXT);")
        .unwrap();
    drop(connection);
    assert!(matches!(
        AgentRegistry::open_shared(&unknown_path, SESSION_SCHEMA_OBJECT_NAMES).await,
        Err(AgentRegistryError::Integrity(_))
    ));

    let partial_path = directory.path().join("partial.db");
    drop(SqliteSessionStore::open(&partial_path).await.unwrap());
    let connection = rusqlite::Connection::open(&partial_path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE schema_migrations (\
                component TEXT NOT NULL,\
                version INTEGER NOT NULL CHECK(version > 0),\
                applied_at INTEGER NOT NULL,\
                PRIMARY KEY(component, version)\
            );",
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        AgentRegistry::open_shared(&partial_path, SESSION_SCHEMA_OBJECT_NAMES).await,
        Err(AgentRegistryError::Integrity(_))
    ));
    let connection = rusqlite::Connection::open(&partial_path).unwrap();
    let installed: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name='agent_definitions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(installed, 0);
}

#[tokio::test]
async fn shared_open_rejects_damage_in_each_owned_schema() {
    for (case, mutation, damage_session) in [
        ("session", "DROP INDEX idx_messages_user;", true),
        (
            "registry",
            "DROP INDEX one_default_model_per_agent_snapshot_v3;",
            false,
        ),
    ] {
        let directory = tempdir().unwrap();
        let path = directory.path().join(format!("{case}.db"));
        drop(SqliteSessionStore::open(&path).await.unwrap());
        drop(
            AgentRegistry::open_shared(&path, SESSION_SCHEMA_OBJECT_NAMES)
                .await
                .unwrap(),
        );
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(mutation).unwrap();
        drop(connection);

        if damage_session {
            assert!(
                SqliteSessionStore::open_shared(&path, REGISTRY_SCHEMA_OBJECT_NAMES)
                    .await
                    .is_err()
            );
        } else {
            assert!(matches!(
                AgentRegistry::open_shared(&path, SESSION_SCHEMA_OBJECT_NAMES).await,
                Err(AgentRegistryError::Integrity(_))
            ));
        }
    }
}

#[tokio::test]
async fn non_current_missing_future_dual_and_damaged_schemas_fail_closed() {
    for (case, mutation) in [
        (
            "missing-ledger",
            "DELETE FROM schema_migrations WHERE component='runtime_registry';",
        ),
        (
            "v1-ledger",
            "UPDATE schema_migrations SET version=1 WHERE component='runtime_registry';",
        ),
        (
            "v2-ledger",
            "UPDATE schema_migrations SET version=2 WHERE component='runtime_registry';",
        ),
        (
            "future-ledger",
            "UPDATE schema_migrations SET version=4 WHERE component='runtime_registry';",
        ),
        (
            "damaged-ledger-time",
            "UPDATE schema_migrations SET applied_at=0 WHERE component='runtime_registry';",
        ),
        (
            "dual-ledger",
            "INSERT INTO schema_migrations(component,version,applied_at) \
             VALUES ('runtime_registry',2,0);",
        ),
        (
            "extra-object",
            "CREATE TABLE unexpected_registry_state(value TEXT);",
        ),
        (
            "damaged-index",
            "DROP INDEX one_default_model_per_agent_snapshot_v3;",
        ),
    ] {
        let directory = tempdir().unwrap();
        let path = directory.path().join(format!("{case}.db"));
        drop(AgentRegistry::open(&path).await.unwrap());
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(mutation).unwrap();
        drop(connection);

        assert!(matches!(
            AgentRegistry::open(path).await,
            Err(AgentRegistryError::Integrity(_))
        ));
    }
}

#[tokio::test]
async fn foreign_key_damage_fails_closed_on_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("foreign-key-damage.db");
    drop(AgentRegistry::open(&path).await.unwrap());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys=OFF;")
        .unwrap();
    connection
        .execute(
            "INSERT INTO provider_registry_heads(provider_id,active_revision,updated_at) \
             VALUES ('missing',1,0)",
            [],
        )
        .unwrap();
    drop(connection);

    assert!(matches!(
        AgentRegistry::open(path).await,
        Err(AgentRegistryError::Integrity(_))
    ));
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
