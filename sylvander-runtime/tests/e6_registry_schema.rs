use rusqlite::Connection;
use sylvander_runtime::agent_registry::{AgentRegistry, AgentRegistryError};

const CURRENT_SCHEMA_VERSION: i64 = 3;
const COMPONENT: &str = "runtime_registry";
const LEGACY_V2_TABLES: &str = "
CREATE TABLE agent_registry_snapshots (
    agent_id TEXT NOT NULL,
    agent_revision INTEGER NOT NULL,
    provider_id TEXT NOT NULL,
    provider_revision INTEGER NOT NULL,
    digest TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(agent_id,agent_revision)
);
CREATE TABLE agent_registry_snapshot_models (
    agent_id TEXT NOT NULL,
    agent_revision INTEGER NOT NULL,
    provider_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    model_revision INTEGER NOT NULL,
    is_default INTEGER NOT NULL,
    PRIMARY KEY(agent_id,agent_revision,provider_id,model_id)
);
CREATE UNIQUE INDEX one_default_model_per_agent_snapshot
    ON agent_registry_snapshot_models(agent_id,agent_revision)
    WHERE is_default=1;
";

#[tokio::test]
async fn new_registry_creates_and_reopens_only_the_current_schema() {
    let directory = tempfile::tempdir().expect("temporary registry");
    let path = directory.path().join("registry.db");
    drop(AgentRegistry::open(&path).await.expect("create registry"));

    let connection = Connection::open(&path).expect("inspect registry");
    let ledger = {
        let mut statement = connection
            .prepare("SELECT component,version FROM schema_migrations ORDER BY component,version")
            .expect("prepare ledger");
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .expect("query ledger")
            .collect::<Result<Vec<_>, _>>()
            .expect("read ledger")
    };
    assert_eq!(ledger, [(COMPONENT.into(), CURRENT_SCHEMA_VERSION)]);
    let snapshot_objects = {
        let mut statement = connection
            .prepare(
                "SELECT type,name FROM sqlite_schema \
                 WHERE name LIKE 'agent_registry_snapshot%' \
                    OR name LIKE 'one_default_model_per_agent_snapshot%' \
                 ORDER BY type,name",
            )
            .expect("prepare snapshot schema inspection");
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("query snapshot schema")
            .collect::<Result<Vec<_>, _>>()
            .expect("read snapshot schema")
    };
    assert_eq!(
        snapshot_objects,
        [
            (
                "index".into(),
                "one_default_model_per_agent_snapshot_v3".into()
            ),
            ("table".into(), "agent_registry_snapshot_models_v3".into()),
            (
                "table".into(),
                "agent_registry_snapshot_providers_v3".into()
            ),
            ("table".into(), "agent_registry_snapshots_v3".into()),
        ]
    );
    drop(connection);

    drop(
        AgentRegistry::open(path)
            .await
            .expect("reopen current registry"),
    );
}

#[tokio::test]
async fn non_current_missing_future_dual_and_damaged_registry_fail_closed() {
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
        ("mixed-v2-snapshot-schema", LEGACY_V2_TABLES),
    ] {
        let directory = tempfile::tempdir().expect("temporary registry");
        let path = directory.path().join(format!("{case}.db"));
        drop(AgentRegistry::open(&path).await.expect("create registry"));
        let connection = Connection::open(&path).expect("open mutation connection");
        connection.execute_batch(mutation).expect("mutate registry");
        drop(connection);

        assert!(matches!(
            AgentRegistry::open(path).await,
            Err(AgentRegistryError::Integrity(_))
        ));
    }
}

#[tokio::test]
async fn external_v2_registry_is_rejected_without_upgrade_or_mutation() {
    let directory = tempfile::tempdir().expect("temporary registry");
    let path = directory.path().join("legacy-v2.db");
    let connection = Connection::open(&path).expect("create external legacy registry");
    connection
        .execute_batch(
            "CREATE TABLE schema_migrations (
                component TEXT NOT NULL,
                version INTEGER NOT NULL,
                applied_at INTEGER NOT NULL,
                PRIMARY KEY(component,version)
            );
            INSERT INTO schema_migrations(component,version,applied_at)
            VALUES ('runtime_registry',2,1);",
        )
        .expect("create external V2 ledger");
    connection
        .execute_batch(LEGACY_V2_TABLES)
        .expect("create external V2 snapshot schema");
    drop(connection);

    assert!(matches!(
        AgentRegistry::open(&path).await,
        Err(AgentRegistryError::Integrity(_))
    ));

    let connection = Connection::open(path).expect("inspect rejected registry");
    let version: i64 = connection
        .query_row(
            "SELECT version FROM schema_migrations WHERE component=?1",
            [COMPONENT],
            |row| row.get(0),
        )
        .expect("legacy ledger remains readable");
    let current_table_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema \
             WHERE type='table' AND name='agent_registry_snapshots_v3')",
            [],
            |row| row.get(0),
        )
        .expect("inspect rejected registry");
    assert_eq!(version, 2);
    assert!(!current_table_exists);
}

#[tokio::test]
async fn foreign_key_damage_fails_closed_on_restart() {
    let directory = tempfile::tempdir().expect("temporary registry");
    let path = directory.path().join("foreign-key-damage.db");
    drop(AgentRegistry::open(&path).await.expect("create registry"));
    let connection = Connection::open(&path).expect("open mutation connection");
    connection
        .execute_batch("PRAGMA foreign_keys=OFF;")
        .expect("disable fixture constraint");
    connection
        .execute(
            "INSERT INTO provider_registry_heads(provider_id,active_revision,updated_at) \
             VALUES ('missing',1,0)",
            [],
        )
        .expect("inject invalid reference");
    drop(connection);

    assert!(matches!(
        AgentRegistry::open(path).await,
        Err(AgentRegistryError::Integrity(_))
    ));
}
