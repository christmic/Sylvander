use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;

use super::coordinated::CoordinatedWorkspaceExecutor;
use sylvander_agent::workspace_executor::{
    WorkspaceCommandOutput, WorkspaceExecutor, WorkspaceExecutorError, WorkspaceTarget,
};

#[derive(Clone)]
struct MemoryWorkspace {
    bytes: Arc<RwLock<Vec<u8>>>,
}

impl MemoryWorkspace {
    fn new(bytes: &[u8]) -> Self {
        Self {
            bytes: Arc::new(RwLock::new(bytes.to_vec())),
        }
    }
}

impl fmt::Debug for MemoryWorkspace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MemoryWorkspace")
    }
}

#[async_trait]
impl WorkspaceExecutor for MemoryWorkspace {
    async fn read_file(
        &self,
        _target: &WorkspaceTarget,
        _relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        Ok(self.bytes.read().unwrap().clone())
    }

    async fn write_file(
        &self,
        _target: &WorkspaceTarget,
        _relative_path: &str,
        content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        *self.bytes.write().unwrap() = content.to_vec();
        Ok(())
    }

    async fn run_command(
        &self,
        _target: &WorkspaceTarget,
        _command: &str,
        _timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        unreachable!("command execution is outside this fixture")
    }
}

#[tokio::test]
async fn concurrent_session_edits_allow_only_one_revision_commit() {
    let inner = Arc::new(MemoryWorkspace::new(b"before"));
    let executor = Arc::new(CoordinatedWorkspaceExecutor::new(inner.clone()));
    let target = WorkspaceTarget {
        id: "shared".into(),
        workspace_path: "/workspace".into(),
        read_only: false,
    };
    let update = executor
        .read_file_for_update(&target, "file.txt", 1024)
        .await
        .expect("read revision");
    let first = {
        let executor = executor.clone();
        let target = target.clone();
        let revision = update.revision.clone();
        tokio::spawn(async move {
            executor
                .write_file_if_revision(&target, "file.txt", &revision, b"first", 1024)
                .await
        })
    };
    let second = {
        let executor = executor.clone();
        let target = target.clone();
        let revision = update.revision;
        tokio::spawn(async move {
            executor
                .write_file_if_revision(&target, "file.txt", &revision, b"second", 1024)
                .await
        })
    };

    let results = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(WorkspaceExecutorError::WriteConflict(_))))
            .count(),
        1
    );
    let final_bytes = inner.bytes.read().unwrap().clone();
    assert!(final_bytes == b"first" || final_bytes == b"second");
}

#[tokio::test]
async fn out_of_band_change_rejects_prepared_revision() {
    let inner = Arc::new(MemoryWorkspace::new(b"before"));
    let executor = CoordinatedWorkspaceExecutor::new(inner.clone());
    let target = WorkspaceTarget {
        id: "shared".into(),
        workspace_path: "/workspace".into(),
        read_only: false,
    };
    let update = executor
        .read_file_for_update(&target, "file.txt", 1024)
        .await
        .expect("read revision");
    *inner.bytes.write().unwrap() = b"outside".to_vec();

    let error = executor
        .write_file_if_revision(&target, "file.txt", &update.revision, b"replacement", 1024)
        .await
        .expect_err("stale revision must fail");
    assert!(matches!(error, WorkspaceExecutorError::WriteConflict(_)));
    assert_eq!(*inner.bytes.read().unwrap(), b"outside");
}
