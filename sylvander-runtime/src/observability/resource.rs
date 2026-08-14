//! Runtime-owned process resource sampling.
//!
//! The monitor observes only the current Runtime process. Network-interface
//! totals are deliberately excluded because they cannot be attributed to this
//! process or one operation; network facts must come from owned I/O adapters.

#[cfg(test)]
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex, PoisonError};
use std::time::Duration;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use super::{RuntimeEvent, RuntimeObservability};

pub const RUNTIME_CPU_DELTA_BUCKET_UPPER_BOUNDS_MILLIS: [u64; 7] = [1, 5, 10, 50, 100, 500, 1_000];
pub const RUNTIME_RSS_BUCKET_UPPER_BOUNDS_BYTES: [u64; 7] = [
    16 * 1024 * 1024,
    32 * 1024 * 1024,
    64 * 1024 * 1024,
    128 * 1024 * 1024,
    256 * 1024 * 1024,
    512 * 1024 * 1024,
    1024 * 1024 * 1024,
];

const RESOURCE_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Whether a resource metric has an objective source in this Runtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RuntimeResourceMetricStatus {
    #[default]
    Unavailable,
    Observed,
    Failed,
}

/// Fixed, non-overlapping distribution for one unsigned resource quantity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeResourceHistogramSnapshot {
    pub count: u64,
    pub total: u64,
    pub max: u64,
    pub bucket_counts: [u64; 8],
}

impl RuntimeResourceHistogramSnapshot {
    fn observe(&mut self, value: u64, bounds: &[u64; 7]) {
        self.count = self.count.saturating_add(1);
        self.total = self.total.saturating_add(value);
        self.max = self.max.max(value);
        let bucket = bounds
            .iter()
            .position(|bound| value <= *bound)
            .unwrap_or(bounds.len());
        self.bucket_counts[bucket] = self.bucket_counts[bucket].saturating_add(1);
    }
}

/// Content-free process and attributed-network resource snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeResourceSnapshot {
    pub cpu_status: RuntimeResourceMetricStatus,
    pub cpu_delta_millis: RuntimeResourceHistogramSnapshot,
    pub rss_status: RuntimeResourceMetricStatus,
    pub rss_bytes: RuntimeResourceHistogramSnapshot,
    pub current_rss_bytes: Option<u64>,
    /// Network remains unavailable until an owned HTTP or sandbox adapter
    /// supplies attributable byte counters.
    pub network_status: RuntimeResourceMetricStatus,
}

#[derive(Default)]
pub(super) struct RuntimeResourceState {
    snapshot: RuntimeResourceSnapshot,
}

impl RuntimeResourceState {
    pub(super) fn observe(&mut self, cpu_delta_millis: Option<u64>, rss_bytes: u64) {
        if let Some(cpu_delta_millis) = cpu_delta_millis {
            self.snapshot.cpu_status = RuntimeResourceMetricStatus::Observed;
            self.snapshot.cpu_delta_millis.observe(
                cpu_delta_millis,
                &RUNTIME_CPU_DELTA_BUCKET_UPPER_BOUNDS_MILLIS,
            );
        }
        self.snapshot.rss_status = RuntimeResourceMetricStatus::Observed;
        self.snapshot
            .rss_bytes
            .observe(rss_bytes, &RUNTIME_RSS_BUCKET_UPPER_BOUNDS_BYTES);
        self.snapshot.current_rss_bytes = Some(rss_bytes);
    }

    pub(super) fn fail(&mut self) {
        self.snapshot.cpu_status = RuntimeResourceMetricStatus::Failed;
        self.snapshot.rss_status = RuntimeResourceMetricStatus::Failed;
    }

    pub(super) const fn snapshot(&self) -> RuntimeResourceSnapshot {
        self.snapshot
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeProcessResourceSample {
    accumulated_cpu_millis: u64,
    rss_bytes: u64,
}

pub(crate) trait RuntimeResourceSampler: Send {
    fn sample(&mut self) -> Result<RuntimeProcessResourceSample, ()>;
}

struct SysinfoRuntimeResourceSampler {
    pid: Pid,
    system: System,
}

impl SysinfoRuntimeResourceSampler {
    fn new() -> Result<Self, ()> {
        Ok(Self {
            pid: sysinfo::get_current_pid().map_err(|_| ())?,
            system: System::new(),
        })
    }
}

impl RuntimeResourceSampler for SysinfoRuntimeResourceSampler {
    fn sample(&mut self) -> Result<RuntimeProcessResourceSample, ()> {
        let pids = [self.pid];
        let refreshed = self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            false,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        if refreshed != 1 {
            return Err(());
        }
        let process = self.system.process(self.pid).ok_or(())?;
        Ok(RuntimeProcessResourceSample {
            accumulated_cpu_millis: process.accumulated_cpu_time(),
            rss_bytes: process.memory(),
        })
    }
}

/// Owned background lifecycle for Runtime process resource sampling.
pub(crate) struct RuntimeResourceMonitor {
    failed: Arc<AtomicBool>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl RuntimeResourceMonitor {
    pub(crate) fn start(observability: RuntimeObservability) -> Result<Self, ()> {
        Self::start_with_sampler(
            observability,
            RESOURCE_SAMPLE_INTERVAL,
            Box::new(SysinfoRuntimeResourceSampler::new()?),
        )
    }

    fn start_with_sampler(
        observability: RuntimeObservability,
        interval: Duration,
        mut sampler: Box<dyn RuntimeResourceSampler>,
    ) -> Result<Self, ()> {
        if interval.is_zero() {
            return Err(());
        }
        let baseline = sampler.sample()?;
        observability.record(RuntimeEvent::ProcessResourcesSampled {
            cpu_delta_millis: None,
            rss_bytes: baseline.rss_bytes,
        });
        let sampler = Arc::new(SyncMutex::new(sampler));
        let failed = Arc::new(AtomicBool::new(false));
        let failed_task = failed.clone();
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut previous_cpu_millis = baseline.accumulated_cpu_millis;
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    _ = ticker.tick() => {
                        let sampler = sampler.clone();
                        let sample_result = tokio::task::spawn_blocking(move || {
                            sampler
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .sample()
                        })
                        .await;
                        let Ok(Ok(sample)) = sample_result else {
                            failed_task.store(true, Ordering::Release);
                            observability.record(RuntimeEvent::ProcessResourceSamplingFailed);
                            break;
                        };
                        let cpu_delta_millis = if sample.accumulated_cpu_millis >= previous_cpu_millis {
                            sample.accumulated_cpu_millis - previous_cpu_millis
                        } else {
                                failed_task.store(true, Ordering::Release);
                                observability.record(RuntimeEvent::ProcessResourceSamplingFailed);
                                break;
                        };
                        previous_cpu_millis = sample.accumulated_cpu_millis;
                        observability.record(RuntimeEvent::ProcessResourcesSampled {
                            cpu_delta_millis: Some(cpu_delta_millis),
                            rss_bytes: sample.rss_bytes,
                        });
                    }
                }
            }
        });
        Ok(Self {
            failed,
            stop: Mutex::new(Some(stop)),
            task: Mutex::new(Some(task)),
        })
    }

    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    pub(crate) async fn shutdown(&self) {
        if let Some(stop) = self.stop.lock().await.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.lock().await.take()
            && task.await.is_err()
        {
            self.failed.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
struct SequenceResourceSampler {
    samples: VecDeque<Result<RuntimeProcessResourceSample, ()>>,
}

#[cfg(test)]
impl RuntimeResourceSampler for SequenceResourceSampler {
    fn sample(&mut self) -> Result<RuntimeProcessResourceSample, ()> {
        self.samples.pop_front().unwrap_or(Err(()))
    }
}

#[cfg(test)]
pub(crate) fn sequence_sampler(
    samples: impl IntoIterator<Item = Result<(u64, u64), ()>>,
) -> Box<dyn RuntimeResourceSampler> {
    Box::new(SequenceResourceSampler {
        samples: samples
            .into_iter()
            .map(|sample| {
                sample.map(
                    |(accumulated_cpu_millis, rss_bytes)| RuntimeProcessResourceSample {
                        accumulated_cpu_millis,
                        rss_bytes,
                    },
                )
            })
            .collect(),
    })
}

#[cfg(test)]
impl RuntimeResourceMonitor {
    pub(crate) fn start_for_test(
        observability: RuntimeObservability,
        interval: Duration,
        sampler: Box<dyn RuntimeResourceSampler>,
    ) -> Result<Self, ()> {
        Self::start_with_sampler(observability, interval, sampler)
    }

    pub(crate) fn fail_for_test(&self) {
        self.failed.store(true, Ordering::Release);
    }
}
