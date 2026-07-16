use std::path::PathBuf;
use std::time::Duration;

use sylvander_agent::tools::{
    MemoryEvidenceCheckpoint, RelationshipMemoryRetentionPolicy, SqliteMemoryMaintenance,
};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::RuntimeError;
use crate::config::MemoryMaintenanceSettings;

pub(crate) struct RuntimeMemoryMaintenancePolicy {
    pub(crate) retention: RelationshipMemoryRetentionPolicy,
    retention_interval: Duration,
    backup_interval: Duration,
    retained_backups: u32,
    max_batches_per_run: u32,
}

impl RuntimeMemoryMaintenancePolicy {
    pub(crate) fn from_settings(
        settings: &MemoryMaintenanceSettings,
    ) -> Result<Self, RuntimeError> {
        let retention = &settings.retention;
        let policy = RelationshipMemoryRetentionPolicy::new(
            retention.revision,
            retention.default_ttl_days,
            retention.max_ttl_days,
            retention.expired_grace_days,
            retention.superseded_retention_days,
            settings.batch_size,
        )
        .map_err(|_| RuntimeError::Config("invalid memory maintenance policy".into()))?;
        if !(60..=86_400).contains(&settings.interval_seconds)
            || !(1..=1_000).contains(&settings.batch_size)
            || !(1..=100).contains(&settings.max_batches_per_run)
            || !(3_600..=604_800).contains(&settings.backup.interval_seconds)
            || !(2..=30).contains(&settings.backup.retained_copies)
        {
            return Err(RuntimeError::Config(
                "invalid memory maintenance policy".into(),
            ));
        }
        Ok(Self {
            retention: policy,
            retention_interval: Duration::from_secs(u64::from(settings.interval_seconds)),
            backup_interval: Duration::from_secs(u64::from(settings.backup.interval_seconds)),
            retained_backups: settings.backup.retained_copies,
            max_batches_per_run: settings.max_batches_per_run,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_interval(mut self, interval: Duration) -> Self {
        self.retention_interval = interval;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_backup_interval(mut self, interval: Duration) -> Self {
        self.backup_interval = interval;
        self
    }
}

pub(crate) struct MemoryMaintenanceTask {
    shutdown: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl MemoryMaintenanceTask {
    pub(crate) fn start(
        maintenance: SqliteMemoryMaintenance,
        policy: RuntimeMemoryMaintenancePolicy,
        data_dir: PathBuf,
    ) -> Self {
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            let now = tokio::time::Instant::now();
            let mut retention_ticks = tokio::time::interval_at(
                now + policy.retention_interval,
                policy.retention_interval,
            );
            let mut backup_ticks =
                tokio::time::interval_at(now + policy.backup_interval, policy.backup_interval);
            retention_ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            backup_ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    result = shutdown_rx.changed() => {
                        if result.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    _ = retention_ticks.tick() => {
                        if let Err(code) = run_bounded(&maintenance, &policy, Some(&shutdown_rx)).await {
                            warn!(failure = code, "memory retention interval failed");
                        }
                    }
                    _ = backup_ticks.tick() => {
                        if let Err(code) = run_backup(&maintenance, &policy, data_dir.clone()).await {
                            warn!(failure = code, "memory backup interval failed");
                        }
                    }
                }
            }
        });
        Self {
            shutdown,
            task: Mutex::new(Some(task)),
        }
    }

    pub(crate) async fn shutdown(&self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.lock().await.take() {
            if task.await.is_err() {
                warn!(failure = "task_join", "memory retention shutdown failed");
            } else {
                info!("memory retention stopped");
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn is_stopped(&self) -> bool {
        self.task.lock().await.is_none()
    }
}

async fn run_backup(
    maintenance: &SqliteMemoryMaintenance,
    policy: &RuntimeMemoryMaintenancePolicy,
    data_dir: PathBuf,
) -> Result<(), &'static str> {
    let mut checkpoint =
        publish_checkpoint(maintenance, data_dir.clone(), policy.retained_backups).await?;
    for _ in 0..policy.max_batches_per_run {
        let handle = maintenance.clone();
        let report = tokio::task::spawn_blocking(move || {
            handle.compact_evidence_after_checkpoint(&checkpoint)
        })
        .await
        .map_err(|_| "checkpoint_task_join")?
        .map_err(|_| "checkpoint_store")?;
        if report.total_deleted_count() == 0 {
            return Ok(());
        }
        // Publish the compacted anchored state before another batch. This is
        // both the next authorization boundary and a restorable final backup
        // if the batch budget is exhausted or shutdown follows immediately.
        checkpoint =
            publish_checkpoint(maintenance, data_dir.clone(), policy.retained_backups).await?;
        tokio::task::yield_now().await;
    }
    Ok(())
}

async fn publish_checkpoint(
    maintenance: &SqliteMemoryMaintenance,
    data_dir: PathBuf,
    retained: u32,
) -> Result<MemoryEvidenceCheckpoint, &'static str> {
    let handle = maintenance.clone();
    let artifact =
        tokio::task::spawn_blocking(move || handle.backup_and_rotate(data_dir, retained))
            .await
            .map_err(|_| "backup_task_join")?
            .map_err(|_| "backup_store")?;
    Ok(MemoryEvidenceCheckpoint::from_verified_backup(artifact))
}

pub(crate) async fn catch_up(
    maintenance: &SqliteMemoryMaintenance,
    policy: &RuntimeMemoryMaintenancePolicy,
) -> Result<(), RuntimeError> {
    if !maintenance
        .has_active_retention_policy()
        .map_err(|_| RuntimeError::Store("memory retention readiness failed".into()))?
    {
        return Ok(());
    }
    run_bounded(maintenance, policy, None)
        .await
        .map_err(|_| RuntimeError::Store("memory retention catch-up failed".into()))
}

async fn run_bounded(
    maintenance: &SqliteMemoryMaintenance,
    policy: &RuntimeMemoryMaintenancePolicy,
    shutdown: Option<&watch::Receiver<bool>>,
) -> Result<(), &'static str> {
    for _ in 0..policy.max_batches_per_run {
        if shutdown.is_some_and(|receiver| *receiver.borrow()) {
            break;
        }
        let handle = maintenance.clone();
        let report = tokio::task::spawn_blocking(move || handle.purge())
            .await
            .map_err(|_| "task_join")?
            .map_err(|_| "store")?;
        if report.total_count() == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        let task =
            MemoryMaintenanceTask::start(store.maintenance(), policy, directory.path().into());
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
        let task =
            MemoryMaintenanceTask::start(store.maintenance(), policy, directory.path().into());
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
}
