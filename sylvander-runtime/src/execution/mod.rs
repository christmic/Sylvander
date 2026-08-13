//! Runtime-owned execution environment composition.
//!
//! Concrete adapters implement Agent's location-neutral workspace port, while
//! `RuntimeExecutionService` owns the immutable mapping from trusted target
//! identifiers to those adapters. Agent runs receive this closed service; they
//! neither register infrastructure nor fall back to a different target.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use sylvander_agent::workspace_executor::{UnavailableExecutor, WorkspaceExecutor};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::task::JoinHandle;
use tokio::task::JoinSet;

const EXECUTION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const EXECUTION_PROBE_INTERVAL: Duration = Duration::from_secs(30);

pub mod container;
mod coordinated;
#[cfg(test)]
#[path = "../../tests/unit/execution_coordination.rs"]
mod coordinated_tests;
mod local;
mod persistent;
mod persistent_container;
pub mod ssh;

pub use container::{ContainerExecutor, ContainerResourcePolicy};
use coordinated::CoordinatedWorkspaceExecutor;
pub(crate) use local::LocalExecutor;
#[cfg(test)]
pub(crate) use persistent::PersistentProcessIsolation;
pub(crate) use persistent::{
    PersistentFilesystemAuthority, PersistentNetworkAuthority, PersistentProcess,
    PersistentProcessAuthority, PersistentProcessEnvironment, PersistentProcessError,
    PersistentProcessOwner, PersistentProcessSpec, PersistentResourceLimits,
    UnavailablePersistentProcessEnvironment,
};
pub(crate) use persistent_container::ContainerPersistentProcessEnvironment;
pub use ssh::SshExecutor;

/// Concrete adapter family selected by trusted Runtime configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionTargetKind {
    Local,
    Ssh,
    Container,
}

/// Truthful availability state without performing network/process I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionTargetStatus {
    Ready,
    Unverified,
    Degraded,
}

/// Content-free execution environment status for operations and diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionTargetHealth {
    pub target_id: String,
    pub kind: ExecutionTargetKind,
    pub status: ExecutionTargetStatus,
    pub filesystem_isolated: bool,
    pub network_denied: bool,
    pub resource_limits: bool,
    pub process_tree: bool,
    pub sandbox_enforced: bool,
    pub probe_failures: u64,
    pub last_probe_succeeded: Option<bool>,
}

#[derive(Clone)]
enum ExecutionTargetProbe {
    Ssh(Arc<SshExecutor>),
    Container(Arc<ContainerExecutor>),
}

impl ExecutionTargetProbe {
    async fn run(&self) -> Result<(), ()> {
        match self {
            Self::Ssh(executor) => executor.probe(EXECUTION_PROBE_TIMEOUT).await,
            Self::Container(executor) => executor.probe(EXECUTION_PROBE_TIMEOUT).await,
        }
    }
}

/// One exact adapter registration consumed during Runtime composition.
pub(crate) struct ExecutionTargetRegistration {
    pub(crate) target_id: String,
    pub(crate) kind: ExecutionTargetKind,
    pub(crate) status: ExecutionTargetStatus,
    pub(crate) executor: Arc<dyn WorkspaceExecutor>,
    persistent_processes: Arc<dyn PersistentProcessEnvironment>,
    probe: Option<ExecutionTargetProbe>,
}

impl ExecutionTargetRegistration {
    pub(crate) fn local(target_id: impl Into<String>) -> Self {
        let target_id = target_id.into();
        Self {
            persistent_processes: Arc::new(UnavailablePersistentProcessEnvironment::new(
                target_id.clone(),
            )),
            target_id,
            kind: ExecutionTargetKind::Local,
            status: ExecutionTargetStatus::Ready,
            executor: Arc::new(LocalExecutor),
            probe: None,
        }
    }

    pub(crate) fn ssh(target_id: impl Into<String>, executor: Arc<SshExecutor>) -> Self {
        let target_id = target_id.into();
        Self {
            persistent_processes: Arc::new(UnavailablePersistentProcessEnvironment::new(
                target_id.clone(),
            )),
            target_id,
            kind: ExecutionTargetKind::Ssh,
            status: ExecutionTargetStatus::Unverified,
            executor: executor.clone(),
            probe: Some(ExecutionTargetProbe::Ssh(executor)),
        }
    }

    pub(crate) fn container(
        target_id: impl Into<String>,
        executor: Arc<ContainerExecutor>,
        persistent_processes: Arc<dyn PersistentProcessEnvironment>,
    ) -> Self {
        Self {
            target_id: target_id.into(),
            kind: ExecutionTargetKind::Container,
            status: ExecutionTargetStatus::Unverified,
            executor: executor.clone(),
            persistent_processes,
            probe: Some(ExecutionTargetProbe::Container(executor)),
        }
    }
}

struct ExecutionTargetEntry {
    executor: Arc<dyn WorkspaceExecutor>,
    persistent_processes: Arc<dyn PersistentProcessEnvironment>,
    probe: Option<ExecutionTargetProbe>,
    health: RwLock<ExecutionTargetHealth>,
}

/// Immutable Runtime registry for concrete execution environments.
#[derive(Clone)]
pub(crate) struct RuntimeExecutionService {
    targets: Arc<HashMap<String, ExecutionTargetEntry>>,
}

impl RuntimeExecutionService {
    /// Build the exact target map selected by Runtime composition.
    pub(crate) fn new(
        registrations: impl IntoIterator<Item = ExecutionTargetRegistration>,
    ) -> Result<Self, ExecutionServiceError> {
        let mut exact = HashMap::new();
        for registration in registrations {
            let target_id = registration.target_id;
            if target_id.trim().is_empty() {
                return Err(ExecutionServiceError::InvalidTargetId);
            }
            let isolation = registration.executor.process_isolation();
            let executor: Arc<dyn WorkspaceExecutor> =
                Arc::new(CoordinatedWorkspaceExecutor::new(registration.executor));
            let entry = ExecutionTargetEntry {
                executor,
                persistent_processes: registration.persistent_processes,
                probe: registration.probe,
                health: RwLock::new(ExecutionTargetHealth {
                    target_id: target_id.clone(),
                    kind: registration.kind,
                    status: registration.status,
                    filesystem_isolated: isolation.filesystem,
                    network_denied: isolation.network_denied,
                    resource_limits: isolation.resource_limits,
                    process_tree: isolation.process_tree,
                    sandbox_enforced: isolation.enforces_sandbox(),
                    probe_failures: 0,
                    last_probe_succeeded: None,
                }),
            };
            if exact.insert(target_id, entry).is_some() {
                return Err(ExecutionServiceError::DuplicateTargetId);
            }
        }
        Ok(Self {
            targets: Arc::new(exact),
        })
    }

    /// Resolve one exact target without an implicit local fallback.
    pub(crate) fn resolve(&self, target_id: &str) -> Option<&Arc<dyn WorkspaceExecutor>> {
        self.targets.get(target_id).map(|entry| &entry.executor)
    }

    /// Resolve the persistent-process adapter for one exact target.
    pub(crate) fn resolve_persistent(
        &self,
        target_id: &str,
    ) -> Option<&Arc<dyn PersistentProcessEnvironment>> {
        self.targets
            .get(target_id)
            .map(|entry| &entry.persistent_processes)
    }

    /// Produce Agent's explicit unavailable adapter for an unknown target.
    pub(crate) fn resolve_or_unavailable(&self, target_id: &str) -> Arc<dyn WorkspaceExecutor> {
        self.resolve(target_id).cloned().unwrap_or_else(|| {
            Arc::new(UnavailableExecutor::new(target_id)) as Arc<dyn WorkspaceExecutor>
        })
    }

    /// Exact local environment used only by direct standalone `AgentRun` builds.
    /// Product composition always replaces it with the configured snapshot.
    pub(crate) fn standalone_local() -> Self {
        Self::new([ExecutionTargetRegistration::local("local")])
            .expect("the fixed local test target is valid")
    }

    /// Return deterministic, content-free target health for diagnostics.
    pub(crate) fn health(&self) -> Vec<ExecutionTargetHealth> {
        let mut health = self
            .targets
            .values()
            .map(|entry| {
                entry
                    .health
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
            .collect::<Vec<_>>();
        health.sort_by(|left, right| left.target_id.cmp(&right.target_id));
        health
    }

    /// Probe all remote/container targets concurrently and update only
    /// content-free health state.
    pub(crate) async fn probe_all(&self) {
        let mut probes = JoinSet::new();
        for (target_id, probe) in self.targets.iter().filter_map(|(target_id, entry)| {
            entry.probe.clone().map(|probe| (target_id.clone(), probe))
        }) {
            probes.spawn(async move { (target_id, probe.run().await) });
        }
        while let Some(joined) = probes.join_next().await {
            let Ok((target_id, result)) = joined else {
                continue;
            };
            let Some(entry) = self.targets.get(&target_id) else {
                continue;
            };
            let mut health = entry
                .health
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            health.last_probe_succeeded = Some(result.is_ok());
            if result.is_ok() {
                health.status = ExecutionTargetStatus::Ready;
            } else {
                health.status = ExecutionTargetStatus::Degraded;
                health.probe_failures = health.probe_failures.saturating_add(1);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        executors: impl IntoIterator<Item = (String, Arc<dyn WorkspaceExecutor>)>,
    ) -> Result<Self, ExecutionServiceError> {
        Self::new(
            executors
                .into_iter()
                .map(|(target_id, executor)| ExecutionTargetRegistration {
                    persistent_processes: Arc::new(UnavailablePersistentProcessEnvironment::new(
                        target_id.clone(),
                    )),
                    target_id,
                    kind: ExecutionTargetKind::Local,
                    status: ExecutionTargetStatus::Unverified,
                    executor,
                    probe: None,
                }),
        )
    }

    #[cfg(test)]
    pub(crate) fn persistent_for_test(
        target_id: impl Into<String>,
        environment: Arc<dyn PersistentProcessEnvironment>,
    ) -> ExecutionTargetRegistration {
        ExecutionTargetRegistration {
            target_id: target_id.into(),
            kind: ExecutionTargetKind::Local,
            status: ExecutionTargetStatus::Ready,
            executor: Arc::new(LocalExecutor),
            persistent_processes: environment,
            probe: None,
        }
    }
}

/// Content-free composition failure for an execution target registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionServiceError {
    InvalidTargetId,
    DuplicateTargetId,
}

/// Runtime-owned background lifecycle for bounded execution target probes.
pub(crate) struct ExecutionHealthTask {
    stop: AsyncMutex<Option<oneshot::Sender<()>>>,
    task: AsyncMutex<Option<JoinHandle<()>>>,
}

impl ExecutionHealthTask {
    pub(crate) fn start(service: RuntimeExecutionService) -> Self {
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                service.probe_all().await;
                tokio::select! {
                    () = tokio::time::sleep(EXECUTION_PROBE_INTERVAL) => {}
                    _ = &mut stopped => break,
                }
            }
        });
        Self {
            stop: AsyncMutex::new(Some(stop)),
            task: AsyncMutex::new(Some(task)),
        }
    }

    pub(crate) async fn shutdown(&self) {
        if let Some(stop) = self.stop.lock().await.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/execution_service.rs"]
mod tests;
