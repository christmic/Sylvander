//! Runtime-owned workspace concurrency boundary.
//!
//! Agent scheduling prevents unsafe overlap within one model batch and Runtime
//! serializes turns within one Session. Neither rule protects two Sessions
//! mounted on the same physical workspace. This decorator owns that final
//! boundary: reads share a workspace lock, all mutation-capable operations take
//! it exclusively, and conditional writes re-read and compare inside the same
//! exclusive section.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::RwLock;

use sylvander_agent::workspace_executor::{
    ProcessIsolation, WorkspaceCommandOutput, WorkspaceCommandProgressSink, WorkspaceExecutor,
    WorkspaceExecutorError, WorkspaceFileRevision, WorkspaceFileUpdate, WorkspaceListRequest,
    WorkspaceListResult, WorkspaceReadResult, WorkspaceSearchRequest, WorkspaceSearchResult,
    WorkspaceTarget,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct WorkspaceKey {
    target_id: String,
    path: PathBuf,
}

/// Decorates one concrete executor with process-local workspace coordination.
#[derive(Clone)]
pub(crate) struct CoordinatedWorkspaceExecutor {
    inner: Arc<dyn WorkspaceExecutor>,
    locks: Arc<Mutex<HashMap<WorkspaceKey, Weak<RwLock<()>>>>>,
}

impl CoordinatedWorkspaceExecutor {
    pub(crate) fn new(inner: Arc<dyn WorkspaceExecutor>) -> Self {
        Self {
            inner,
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn workspace_lock(&self, target: &WorkspaceTarget) -> Arc<RwLock<()>> {
        let key = WorkspaceKey {
            target_id: target.id.clone(),
            path: target.workspace_path.clone(),
        };
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = Arc::new(RwLock::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }
}

impl fmt::Debug for CoordinatedWorkspaceExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CoordinatedWorkspaceExecutor")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl WorkspaceExecutor for CoordinatedWorkspaceExecutor {
    fn process_isolation(&self) -> ProcessIsolation {
        self.inner.process_isolation()
    }

    fn select_mount_target(
        &self,
        target: &WorkspaceTarget,
        reference: Option<&str>,
    ) -> Result<WorkspaceTarget, WorkspaceExecutorError> {
        self.inner.select_mount_target(target, reference)
    }

    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.read().await;
        self.inner.read_file(target, relative_path).await
    }

    async fn read_file_bounded(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        max_bytes: usize,
    ) -> Result<WorkspaceReadResult, WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.read().await;
        self.inner
            .read_file_bounded(target, relative_path, max_bytes)
            .await
    }

    async fn read_file_for_update(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        max_bytes: usize,
    ) -> Result<WorkspaceFileUpdate, WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.read().await;
        let read = self
            .inner
            .read_file_bounded(target, relative_path, max_bytes)
            .await?;
        let revision = WorkspaceFileRevision::for_bytes(&read.bytes);
        Ok(WorkspaceFileUpdate { read, revision })
    }

    async fn write_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.write().await;
        self.inner.write_file(target, relative_path, content).await
    }

    async fn write_file_if_revision(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        expected: &WorkspaceFileRevision,
        content: &[u8],
        max_bytes: usize,
    ) -> Result<(), WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.write().await;
        let current = self
            .inner
            .read_file_bounded(target, relative_path, max_bytes)
            .await?;
        if current.truncated || WorkspaceFileRevision::for_bytes(&current.bytes) != *expected {
            return Err(WorkspaceExecutorError::WriteConflict(
                relative_path.to_owned(),
            ));
        }
        self.inner.write_file(target, relative_path, content).await
    }

    async fn run_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.write().await;
        self.inner.run_command(target, command, timeout).await
    }

    async fn run_command_with_environment(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        environment: &BTreeMap<String, String>,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.write().await;
        self.inner
            .run_command_with_environment(target, command, timeout, environment)
            .await
    }

    async fn run_command_streaming(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        progress: WorkspaceCommandProgressSink,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.write().await;
        self.inner
            .run_command_streaming(target, command, timeout, progress)
            .await
    }

    async fn run_command_streaming_with_environment(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        environment: &BTreeMap<String, String>,
        progress: WorkspaceCommandProgressSink,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.write().await;
        self.inner
            .run_command_streaming_with_environment(target, command, timeout, environment, progress)
            .await
    }

    async fn run_read_only_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.read().await;
        self.inner
            .run_read_only_command(target, command, timeout)
            .await
    }

    async fn list(
        &self,
        target: &WorkspaceTarget,
        request: WorkspaceListRequest,
    ) -> Result<WorkspaceListResult, WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.read().await;
        self.inner.list(target, request).await
    }

    async fn search(
        &self,
        target: &WorkspaceTarget,
        request: WorkspaceSearchRequest,
    ) -> Result<WorkspaceSearchResult, WorkspaceExecutorError> {
        let lock = self.workspace_lock(target);
        let _guard = lock.read().await;
        self.inner.search(target, request).await
    }
}
