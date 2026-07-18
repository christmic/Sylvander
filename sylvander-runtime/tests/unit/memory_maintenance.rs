use super::*;
use std::collections::BTreeSet;

use sylvander_agent::tools::{
    MemoryBackupManifest, MemoryIntegrityConfig, RelationshipMemoryRetentionPolicy,
    SqliteMemoryStore,
};

fn protected_store(directory: &std::path::Path) -> SqliteMemoryStore {
    let store = SqliteMemoryStore::open_with_integrity(
        directory.join("memory.db"),
        RelationshipMemoryRetentionPolicy::default(),
        MemoryIntegrityConfig::new(
            directory.join("memory.anchor"),
            b"0123456789abcdef0123456789abcdef",
        )
        .unwrap(),
    )
    .unwrap();
    store
        .maintenance()
        .activate_staged_retention_policy()
        .unwrap();
    store
}

#[test]
fn settings_map_exact_revision_and_maximum_batch() {
    let mut settings = MemoryMaintenanceSettings::default();
    settings.retention.revision = 7;
    settings.batch_size = 1_000;
    let policy = RuntimeMemoryMaintenancePolicy::from_settings(&settings).unwrap();
    assert_eq!(policy.retention.revision(), 7);
    assert_eq!(policy.retention.batch_limit(), 1_000);
    assert_eq!(policy.retained_backups, 7);
    settings.batch_size = 1_001;
    assert!(RuntimeMemoryMaintenancePolicy::from_settings(&settings).is_err());
}

fn complete_backup_names(data_dir: &std::path::Path) -> BTreeSet<String> {
    let directory = data_dir.join("memory-backups");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return BTreeSet::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".manifest.json")?;
            entry
                .path()
                .with_file_name(format!("{id}.sqlite3"))
                .is_file()
                .then_some(id.to_owned())
        })
        .collect()
}

async fn wait_for(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn scheduled_rotation_restarts_and_shutdown_stops_future_backups() {
    let directory = tempfile::tempdir().unwrap();
    let store = protected_store(directory.path());
    let mut settings = MemoryMaintenanceSettings::default();
    settings.backup.retained_copies = 2;
    let policy = RuntimeMemoryMaintenancePolicy::from_settings(&settings)
        .unwrap()
        .with_interval(Duration::from_hours(1))
        .with_backup_interval(Duration::from_millis(10));
    let task = MemoryMaintenanceTask::start(store.maintenance(), policy, directory.path().into());
    wait_for(|| complete_backup_names(directory.path()).len() == 2).await;
    task.shutdown().await;
    let stopped = complete_backup_names(directory.path());
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(complete_backup_names(directory.path()), stopped);

    let policy = RuntimeMemoryMaintenancePolicy::from_settings(&settings)
        .unwrap()
        .with_interval(Duration::from_hours(1))
        .with_backup_interval(Duration::from_millis(10));
    let restarted =
        MemoryMaintenanceTask::start(store.maintenance(), policy, directory.path().into());
    wait_for(|| complete_backup_names(directory.path()) != stopped).await;
    restarted.shutdown().await;
    assert_eq!(complete_backup_names(directory.path()).len(), 2);
}

#[tokio::test]
async fn backup_failure_is_content_safe_and_retries_next_interval() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let store = protected_store(directory.path());
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER private_failure AFTER INSERT ON relationship_memories BEGIN SELECT 1; END;",
        )
        .unwrap();
    let policy =
        RuntimeMemoryMaintenancePolicy::from_settings(&MemoryMaintenanceSettings::default())
            .unwrap()
            .with_interval(Duration::from_hours(1))
            .with_backup_interval(Duration::from_millis(10));
    assert_eq!(
        run_backup(&store.maintenance(), &policy, directory.path().into()).await,
        Err("backup_store")
    );
    let task = MemoryMaintenanceTask::start(store.maintenance(), policy, directory.path().into());
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(complete_backup_names(directory.path()).is_empty());
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch("DROP TRIGGER private_failure")
        .unwrap();
    wait_for(|| !complete_backup_names(directory.path()).is_empty()).await;
    task.shutdown().await;
}

#[tokio::test]
async fn scheduled_backup_bounds_evidence_and_publishes_the_final_epoch() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    let store = protected_store(directory.path());
    for _ in 0..5 {
        store.maintenance().purge().unwrap();
    }

    let policy =
        RuntimeMemoryMaintenancePolicy::from_settings(&MemoryMaintenanceSettings::default())
            .unwrap();
    run_backup(&store.maintenance(), &policy, directory.path().into())
        .await
        .unwrap();

    let connection = rusqlite::Connection::open(&database).unwrap();
    let counts: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM relationship_memory_audit), (SELECT COUNT(*) FROM relationship_memory_retention_runs), (SELECT COUNT(*) FROM relationship_memory_retention_batches), (SELECT COUNT(*) FROM relationship_memory_checkpoint_state)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(counts, (0, 1, 1, 1));

    let anchor: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("memory.anchor")).unwrap())
            .unwrap();
    let current_epoch = anchor["epoch"].as_u64().unwrap();
    let has_current_backup = std::fs::read_dir(directory.path().join("memory-backups"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|value| value == "json")
        })
        .filter_map(|entry| std::fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<MemoryBackupManifest>(&bytes).ok())
        .any(|manifest| manifest.integrity_epoch == current_epoch);
    assert!(has_current_backup);
}
