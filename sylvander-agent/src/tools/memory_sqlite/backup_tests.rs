use super::*;
use crate::tools::memory::{MemoryAppend, MemoryExecutionContext, MemoryFilter, MemoryStore};
use sylvander_protocol::SessionContext;

fn worker() -> MemoryExecutionContext {
    MemoryExecutionContext::application_worker(&SessionContext::new("alice", "agent-a", "session"))
}

async fn backup_fixture() -> (tempfile::TempDir, MemoryBackupArtifact) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteMemoryStore::open(directory.path().join("source.db")).unwrap();
    store
        .append_relationship(&worker(), MemoryAppend::new("backup-only value"))
        .await
        .unwrap();
    let artifact = store
        .maintenance()
        .backup_to_data_dir(directory.path())
        .unwrap();
    (directory, artifact)
}

fn verified_pair_count(data_dir: &Path) -> usize {
    verified_backup_pairs(&data_dir.join("memory-backups"))
        .unwrap()
        .len()
}

async fn create_live(path: &Path) -> Vec<u8> {
    let store = SqliteMemoryStore::open(path).unwrap();
    store
        .append_relationship(&worker(), MemoryAppend::new("live-original value"))
        .await
        .unwrap();
    drop(store);
    std::fs::read(path).unwrap()
}

fn assert_no_restore_temps(directory: &Path) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy();
        assert!(!name.starts_with(".memory-restore-"));
        assert!(!name.starts_with(".memory-rollback-"));
    }
}

#[tokio::test]
async fn online_backup_and_explicit_offline_restore_round_trip() {
    let (directory, artifact) = backup_fixture().await;
    assert!(
        artifact
            .database_path
            .starts_with(directory.path().join("memory-backups"))
    );
    assert_eq!(artifact.manifest.schema_version, SCHEMA_VERSION);
    assert!(artifact.manifest.size_bytes > 0);
    assert_eq!(artifact.manifest.sha256.len(), 64);
    let manifest_json = std::fs::read_to_string(&artifact.manifest_path).unwrap();
    assert!(!manifest_json.contains("backup-only value"));
    assert!(!manifest_json.contains("alice"));

    let live = directory.path().join("live.db");
    create_live(&live).await;
    SqliteMemoryAdmin::restore_offline(&live, &artifact.database_path, &artifact.manifest_path)
        .unwrap();
    assert_no_restore_temps(directory.path());
    let restored = SqliteMemoryStore::open(&live).unwrap();
    assert_eq!(
        restored
            .search_relationship(&worker(), "backup-only", MemoryFilter::default())
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        restored
            .search_relationship(&worker(), "live-original", MemoryFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn post_replace_failure_rolls_back_original_and_cleans_private_files() {
    let (directory, artifact) = backup_fixture().await;
    let live = directory.path().join("live.db");
    let original = create_live(&live).await;
    let error = restore_offline_impl(
        &live,
        &artifact.database_path,
        &artifact.manifest_path,
        true,
    )
    .unwrap_err();
    assert_eq!(error, MemoryRestoreError::Rejected);
    assert_eq!(std::fs::read(&live).unwrap(), original);
    assert_no_restore_temps(directory.path());
}

#[tokio::test]
async fn corrupt_old_interrupted_and_fk_invalid_artifacts_preserve_live_database() {
    let (directory, artifact) = backup_fixture().await;
    let live = directory.path().join("live.db");
    let original = create_live(&live).await;

    let corrupt = directory.path().join("corrupt.db");
    std::fs::copy(&artifact.database_path, &corrupt).unwrap();
    let mut bytes = std::fs::read(&corrupt).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x5a;
    std::fs::write(&corrupt, bytes).unwrap();
    assert_eq!(
        SqliteMemoryAdmin::restore_offline(&live, &corrupt, &artifact.manifest_path),
        Err(MemoryRestoreError::Rejected)
    );
    assert_eq!(std::fs::read(&live).unwrap(), original);

    let bad_manifest = directory.path().join("bad.manifest.json");
    let mut manifest = artifact.manifest.clone();
    manifest.sha256 = "00".repeat(32);
    std::fs::write(&bad_manifest, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert_eq!(
        SqliteMemoryAdmin::restore_offline(&live, &artifact.database_path, &bad_manifest),
        Err(MemoryRestoreError::Rejected)
    );
    assert_eq!(std::fs::read(&live).unwrap(), original);

    let old = directory.path().join("old.db");
    std::fs::copy(&artifact.database_path, &old).unwrap();
    let connection = Connection::open(&old).unwrap();
    connection
        .execute(
            "UPDATE memory_schema_migrations SET version = 2 WHERE component = 'relationship_memory'",
            [],
        )
        .unwrap();
    drop(connection);
    let old_manifest = refreshed_manifest(directory.path(), "old", &old, &artifact.manifest);
    assert_eq!(
        SqliteMemoryAdmin::restore_offline(&live, &old, &old_manifest),
        Err(MemoryRestoreError::Rejected)
    );
    assert_eq!(std::fs::read(&live).unwrap(), original);

    let missing_manifest = directory.path().join("interrupted.manifest.tmp");
    assert_eq!(
        SqliteMemoryAdmin::restore_offline(&live, &artifact.database_path, &missing_manifest),
        Err(MemoryRestoreError::Rejected)
    );

    let invalid_fk = directory.path().join("invalid-fk.db");
    std::fs::copy(&artifact.database_path, &invalid_fk).unwrap();
    let connection = Connection::open(&invalid_fk).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = OFF")
        .unwrap();
    connection.execute(
        "INSERT INTO relationship_memory_retention_batches (batch_id, run_id, occurred_at, attempted_limit, expired_count, superseded_count) VALUES ('bad', 'missing-run', 1, 1, 0, 0)",
        [],
    ).unwrap();
    drop(connection);
    let fk_manifest = refreshed_manifest(directory.path(), "fk", &invalid_fk, &artifact.manifest);
    assert_eq!(
        SqliteMemoryAdmin::restore_offline(&live, &invalid_fk, &fk_manifest),
        Err(MemoryRestoreError::Rejected)
    );
    assert_eq!(std::fs::read(&live).unwrap(), original);

    let mut sidecar = live.as_os_str().to_owned();
    sidecar.push("-wal");
    std::fs::write(&sidecar, b"interrupted").unwrap();
    assert_eq!(
        SqliteMemoryAdmin::restore_offline(&live, &artifact.database_path, &artifact.manifest_path),
        Err(MemoryRestoreError::Rejected)
    );
    assert_eq!(std::fs::read(&live).unwrap(), original);
    assert_no_restore_temps(directory.path());
}

fn refreshed_manifest(
    directory: &Path,
    name: &str,
    database: &Path,
    template: &MemoryBackupManifest,
) -> PathBuf {
    let (size_bytes, sha256) = digest_file(database).unwrap();
    let manifest = MemoryBackupManifest {
        size_bytes,
        sha256,
        ..template.clone()
    };
    let path = directory.join(format!("{name}.manifest.json"));
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    path
}

#[test]
fn rotation_survives_restart_and_keeps_only_newest_verified_pairs() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("source.db");
    for expected in 1..=2 {
        let store = SqliteMemoryStore::open(&database).unwrap();
        store
            .maintenance()
            .backup_and_rotate(directory.path(), 2)
            .unwrap();
        drop(store);
        assert_eq!(verified_pair_count(directory.path()), expected);
    }
    for _ in 0..3 {
        let store = SqliteMemoryStore::open(&database).unwrap();
        store
            .maintenance()
            .backup_and_rotate(directory.path(), 2)
            .unwrap();
        drop(store);
        assert_eq!(verified_pair_count(directory.path()), 2);
    }
}

#[test]
fn rotation_ignores_temporary_orphan_and_invalid_pairs() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteMemoryStore::open(directory.path().join("source.db")).unwrap();
    let first = store
        .maintenance()
        .backup_and_rotate(directory.path(), 2)
        .unwrap();
    let backups = directory.path().join("memory-backups");
    std::fs::copy(
        &first.database_path,
        backups.join("relationship-memory-orphan.sqlite3"),
    )
    .unwrap();
    std::fs::copy(
        &first.manifest_path,
        backups.join("relationship-memory-missing-db.manifest.json"),
    )
    .unwrap();
    std::fs::write(
        backups.join(".relationship-memory-temp.sqlite3.tmp"),
        b"partial",
    )
    .unwrap();
    let invalid_database = backups.join("relationship-memory-invalid.sqlite3");
    std::fs::copy(&first.database_path, &invalid_database).unwrap();
    let invalid_manifest = backups.join("relationship-memory-invalid.manifest.json");
    let mut manifest = first.manifest.clone();
    manifest.sha256 = "00".repeat(32);
    std::fs::write(invalid_manifest, serde_json::to_vec(&manifest).unwrap()).unwrap();

    store
        .maintenance()
        .backup_and_rotate(directory.path(), 2)
        .unwrap();
    assert_eq!(verified_pair_count(directory.path()), 2);
    assert!(backups.join("relationship-memory-orphan.sqlite3").exists());
    assert!(
        backups
            .join("relationship-memory-missing-db.manifest.json")
            .exists()
    );
}

#[test]
fn failed_new_backup_preserves_every_existing_verified_pair() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteMemoryStore::open(directory.path().join("source.db")).unwrap();
    for _ in 0..2 {
        store
            .maintenance()
            .backup_and_rotate(directory.path(), 2)
            .unwrap();
    }
    store
        .with_connection(|connection| {
            connection
                .execute_batch("CREATE TRIGGER unexpected_backup_object AFTER INSERT ON relationship_memories BEGIN SELECT 1; END;")
                .map_err(|_| backup_error())
        })
        .unwrap();

    let error = store
        .maintenance()
        .backup_and_rotate(directory.path(), 2)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "store error: memory backup operation failed"
    );
    assert_eq!(verified_pair_count(directory.path()), 2);
}
