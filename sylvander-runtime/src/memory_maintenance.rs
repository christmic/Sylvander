use std::time::Duration;

use sylvander_agent::tools::{RelationshipMemoryRetentionPolicy, SqliteMemoryMaintenance};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::RuntimeError;
use crate::config::MemoryMaintenanceSettings;

pub(crate) struct RuntimeMemoryMaintenancePolicy {
    pub(crate) retention: RelationshipMemoryRetentionPolicy,
    interval: Duration,
    batch_size: u32,
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
        {
            return Err(RuntimeError::Config(
                "invalid memory maintenance policy".into(),
            ));
        }
        Ok(Self {
            retention: policy,
            interval: Duration::from_secs(u64::from(settings.interval_seconds)),
            batch_size: settings.batch_size,
            max_batches_per_run: settings.max_batches_per_run,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
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
    ) -> Self {
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = shutdown_rx.changed() => {
                        if result.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    () = tokio::time::sleep(policy.interval) => {
                        if let Err(code) = run_bounded(&maintenance, &policy, Some(&shutdown_rx)).await {
                            warn!(failure = code, "memory retention interval failed");
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

pub(crate) async fn catch_up(
    maintenance: &SqliteMemoryMaintenance,
    policy: &RuntimeMemoryMaintenancePolicy,
) -> Result<(), RuntimeError> {
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
        if report.total_count() < policy.batch_size {
            break;
        }
        tokio::task::yield_now().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_map_exact_revision_and_maximum_batch() {
        let mut settings = MemoryMaintenanceSettings::default();
        settings.retention.revision = 7;
        settings.batch_size = 1_000;
        let policy = RuntimeMemoryMaintenancePolicy::from_settings(&settings).unwrap();
        assert_eq!(policy.retention.revision(), 7);
        assert_eq!(policy.retention.batch_limit(), 1_000);
        settings.batch_size = 1_001;
        assert!(RuntimeMemoryMaintenancePolicy::from_settings(&settings).is_err());
    }
}
