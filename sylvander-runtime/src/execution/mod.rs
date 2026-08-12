//! Runtime-owned execution environment composition.
//!
//! Concrete adapters implement Agent's location-neutral workspace port, while
//! [`RuntimeExecutionService`] owns the immutable mapping from trusted target
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
mod local;
pub mod ssh;

pub use container::{ContainerExecutor, ContainerResourcePolicy};
pub(crate) use local::LocalExecutor;
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
    probe: Option<ExecutionTargetProbe>,
}

impl ExecutionTargetRegistration {
    pub(crate) fn local(target_id: impl Into<String>) -> Self {
        Self {
            target_id: target_id.into(),
            kind: ExecutionTargetKind::Local,
            status: ExecutionTargetStatus::Ready,
            executor: Arc::new(LocalExecutor),
            probe: None,
        }
    }

    pub(crate) fn ssh(target_id: impl Into<String>, executor: Arc<SshExecutor>) -> Self {
        Self {
            target_id: target_id.into(),
            kind: ExecutionTargetKind::Ssh,
            status: ExecutionTargetStatus::Unverified,
            executor: executor.clone(),
            probe: Some(ExecutionTargetProbe::Ssh(executor)),
        }
    }

    pub(crate) fn container(
        target_id: impl Into<String>,
        executor: Arc<ContainerExecutor>,
    ) -> Self {
        Self {
            target_id: target_id.into(),
            kind: ExecutionTargetKind::Container,
            status: ExecutionTargetStatus::Unverified,
            executor: executor.clone(),
            probe: Some(ExecutionTargetProbe::Container(executor)),
        }
    }
}

struct ExecutionTargetEntry {
    executor: Arc<dyn WorkspaceExecutor>,
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
            let entry = ExecutionTargetEntry {
                executor: registration.executor,
                probe: registration.probe,
                health: RwLock::new(ExecutionTargetHealth {
                    target_id: target_id.clone(),
                    kind: registration.kind,
                    status: registration.status,
                    filesystem_isolated: isolation.filesystem,
                    network_denied: isolation.network_denied,
                    resource_limits: isolation.resource_limits,
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
                    target_id,
                    kind: ExecutionTargetKind::Local,
                    status: ExecutionTargetStatus::Unverified,
                    executor,
                    probe: None,
                }),
        )
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
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::time::Duration;

    use sylvander_agent::workspace_executor::WorkspaceExecutor;

    use super::{
        ContainerExecutor, ExecutionServiceError, ExecutionTargetKind, ExecutionTargetRegistration,
        ExecutionTargetStatus, LocalExecutor, RuntimeExecutionService,
    };

    #[test]
    fn target_registry_rejects_blank_and_duplicate_identifiers() {
        let local = Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>;
        assert!(matches!(
            RuntimeExecutionService::new([ExecutionTargetRegistration {
                target_id: " ".into(),
                kind: ExecutionTargetKind::Local,
                status: ExecutionTargetStatus::Unverified,
                executor: local.clone(),
                probe: None,
            }]),
            Err(ExecutionServiceError::InvalidTargetId)
        ));
        assert!(matches!(
            RuntimeExecutionService::new([
                ExecutionTargetRegistration {
                    target_id: "local".into(),
                    kind: ExecutionTargetKind::Local,
                    status: ExecutionTargetStatus::Unverified,
                    executor: local.clone(),
                    probe: None,
                },
                ExecutionTargetRegistration {
                    target_id: "local".into(),
                    kind: ExecutionTargetKind::Local,
                    status: ExecutionTargetStatus::Unverified,
                    executor: local,
                    probe: None,
                },
            ]),
            Err(ExecutionServiceError::DuplicateTargetId)
        ));
    }

    #[test]
    fn health_is_sorted_and_never_calls_unconfined_targets_sandboxes() {
        let service = RuntimeExecutionService::new([
            ExecutionTargetRegistration {
                target_id: "ssh:build".into(),
                kind: ExecutionTargetKind::Ssh,
                status: ExecutionTargetStatus::Unverified,
                executor: Arc::new(LocalExecutor),
                probe: None,
            },
            ExecutionTargetRegistration {
                target_id: "container:review".into(),
                kind: ExecutionTargetKind::Container,
                status: ExecutionTargetStatus::Unverified,
                executor: Arc::new(ContainerExecutor::new("docker", "review:latest").unwrap()),
                probe: None,
            },
            ExecutionTargetRegistration {
                target_id: "local".into(),
                kind: ExecutionTargetKind::Local,
                status: ExecutionTargetStatus::Ready,
                executor: Arc::new(LocalExecutor),
                probe: None,
            },
        ])
        .unwrap();

        let health = service.health();
        assert_eq!(
            health
                .iter()
                .map(|target| target.target_id.as_str())
                .collect::<Vec<_>>(),
            ["container:review", "local", "ssh:build"]
        );
        assert!(health[0].sandbox_enforced);
        assert!(!health[1].sandbox_enforced);
        assert!(!health[2].sandbox_enforced);
        assert_eq!(health[0].status, ExecutionTargetStatus::Unverified);
        assert_eq!(health[1].status, ExecutionTargetStatus::Ready);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probes_promote_success_and_retain_content_free_failure_counts() {
        let directory = tempfile::TempDir::new().unwrap();
        let executable = directory.path().join("container-runtime");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let available = Arc::new(ContainerExecutor::new(&executable, "review:latest").unwrap());
        let missing = Arc::new(
            ContainerExecutor::new(directory.path().join("missing-runtime"), "review:latest")
                .unwrap(),
        );
        let service = RuntimeExecutionService::new([
            ExecutionTargetRegistration::container("container:ok", available),
            ExecutionTargetRegistration::container("container:missing", missing),
        ])
        .unwrap();

        service.probe_all().await;
        service.probe_all().await;
        let health = service.health();
        let missing = health
            .iter()
            .find(|target| target.target_id == "container:missing")
            .unwrap();
        let ready = health
            .iter()
            .find(|target| target.target_id == "container:ok")
            .unwrap();
        assert_eq!(missing.status, ExecutionTargetStatus::Degraded);
        assert_eq!(missing.probe_failures, 2);
        assert_eq!(missing.last_probe_succeeded, Some(false));
        assert_eq!(ready.status, ExecutionTargetStatus::Ready);
        assert_eq!(ready.probe_failures, 0);
        assert_eq!(ready.last_probe_succeeded, Some(true));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_probe_is_owned_and_shutdown_is_joined() {
        let directory = tempfile::TempDir::new().unwrap();
        let missing = Arc::new(
            ContainerExecutor::new(directory.path().join("missing-runtime"), "review:latest")
                .unwrap(),
        );
        let service = RuntimeExecutionService::new([ExecutionTargetRegistration::container(
            "container:missing",
            missing,
        )])
        .unwrap();
        let task = super::ExecutionHealthTask::start(service.clone());
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if service.health()[0].status == ExecutionTargetStatus::Degraded {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        task.shutdown().await;
        assert!(task.stop.lock().await.is_none());
        assert!(task.task.lock().await.is_none());
    }
}
