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

/// Immutable Runtime registry for concrete execution environments.
#[derive(Clone)]
pub(crate) struct RuntimeExecutionService {
    executors: Arc<HashMap<String, Arc<dyn WorkspaceExecutor>>>,
}

impl RuntimeExecutionService {
    /// Build the exact target map selected by Runtime composition.
    pub(crate) fn new(
        executors: impl IntoIterator<Item = (String, Arc<dyn WorkspaceExecutor>)>,
    ) -> Result<Self, ExecutionServiceError> {
        let mut exact = HashMap::new();
        for (target_id, executor) in executors {
            if target_id.trim().is_empty() {
                return Err(ExecutionServiceError::InvalidTargetId);
            }
            if exact.insert(target_id, executor).is_some() {
                return Err(ExecutionServiceError::DuplicateTargetId);
            }
        }
        Ok(Self {
            executors: Arc::new(exact),
        })
    }

    /// Resolve one exact target without an implicit local fallback.
    pub(crate) fn resolve(&self, target_id: &str) -> Option<&Arc<dyn WorkspaceExecutor>> {
        self.executors.get(target_id)
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
        Self::new([(
            "local".to_owned(),
            Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>,
        )])
        .expect("the fixed local test target is valid")
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

    use super::{ExecutionServiceError, LocalExecutor, RuntimeExecutionService};

    #[test]
    fn target_registry_rejects_blank_and_duplicate_identifiers() {
        let local = Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>;
        assert!(matches!(
            RuntimeExecutionService::new([(" ".into(), local.clone())]),
            Err(ExecutionServiceError::InvalidTargetId)
        ));
        assert!(matches!(
            RuntimeExecutionService::new([
                ("local".into(), local.clone()),
                ("local".into(), local),
            ]),
            Err(ExecutionServiceError::DuplicateTargetId)
        ));
    }
}
