//! Runtime-owned execution environment composition.
//!
//! Concrete adapters implement Agent's location-neutral workspace port, while
//! [`RuntimeExecutionService`] owns the immutable mapping from trusted target
//! identifiers to those adapters. Agent runs receive this closed service; they
//! neither register infrastructure nor fall back to a different target.

use std::collections::HashMap;
use std::sync::Arc;

use sylvander_agent::workspace_executor::{UnavailableExecutor, WorkspaceExecutor};

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
}

/// One exact adapter registration consumed during Runtime composition.
pub(crate) struct ExecutionTargetRegistration {
    pub(crate) target_id: String,
    pub(crate) kind: ExecutionTargetKind,
    pub(crate) status: ExecutionTargetStatus,
    pub(crate) executor: Arc<dyn WorkspaceExecutor>,
}

struct ExecutionTargetEntry {
    executor: Arc<dyn WorkspaceExecutor>,
    health: ExecutionTargetHealth,
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
                health: ExecutionTargetHealth {
                    target_id: target_id.clone(),
                    kind: registration.kind,
                    status: registration.status,
                    filesystem_isolated: isolation.filesystem,
                    network_denied: isolation.network_denied,
                    resource_limits: isolation.resource_limits,
                    sandbox_enforced: isolation.enforces_sandbox(),
                },
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
        Self::new([ExecutionTargetRegistration {
            target_id: "local".to_owned(),
            kind: ExecutionTargetKind::Local,
            status: ExecutionTargetStatus::Ready,
            executor: Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>,
        }])
        .expect("the fixed local test target is valid")
    }

    /// Return deterministic, content-free target health for diagnostics.
    pub(crate) fn health(&self) -> Vec<ExecutionTargetHealth> {
        let mut health = self
            .targets
            .values()
            .map(|entry| entry.health.clone())
            .collect::<Vec<_>>();
        health.sort_by(|left, right| left.target_id.cmp(&right.target_id));
        health
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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
                },
                ExecutionTargetRegistration {
                    target_id: "local".into(),
                    kind: ExecutionTargetKind::Local,
                    status: ExecutionTargetStatus::Unverified,
                    executor: local,
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
            },
            ExecutionTargetRegistration {
                target_id: "container:review".into(),
                kind: ExecutionTargetKind::Container,
                status: ExecutionTargetStatus::Unverified,
                executor: Arc::new(ContainerExecutor::new("docker", "review:latest").unwrap()),
            },
            ExecutionTargetRegistration {
                target_id: "local".into(),
                kind: ExecutionTargetKind::Local,
                status: ExecutionTargetStatus::Ready,
                executor: Arc::new(LocalExecutor),
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
}
