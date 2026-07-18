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
#[path = "../tests/unit/memory_maintenance.rs"]
mod tests;
