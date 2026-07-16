//! Location-neutral workspace operations used by coding tools.

use std::fmt::Debug;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;

/// A workspace mounted on an execution target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceTarget {
    pub id: String,
    pub workspace_path: PathBuf,
    pub read_only: bool,
}

impl WorkspaceTarget {
    #[must_use]
    pub fn local(workspace_path: impl Into<PathBuf>, read_only: bool) -> Self {
        Self {
            id: "local".into(),
            workspace_path: workspace_path.into(),
            read_only,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceExecutorError {
    #[error("execution target `{0}` is unavailable on this server")]
    Unavailable(String),
    #[error("execution target `{0}` is read-only")]
    ReadOnly(String),
    #[error("invalid workspace path: {0}")]
    InvalidPath(String),
    #[error("workspace operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("workspace command timed out after {0:?}")]
    Timeout(Duration),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceCommandOutput {
    pub success: bool,
    pub status_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Transport-neutral operations needed by the built-in coding tools.
#[async_trait]
pub trait WorkspaceExecutor: Send + Sync + Debug {
    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError>;

    async fn write_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        content: &[u8],
    ) -> Result<(), WorkspaceExecutorError>;

    async fn run_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError>;
}

/// Executor for a workspace available on the Sylvander server's filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalExecutor;

#[async_trait]
impl WorkspaceExecutor for LocalExecutor {
    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        let path = resolve_existing(target, relative_path).await?;
        Ok(tokio::fs::read(path).await?)
    }

    async fn write_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        ensure_writable(target)?;
        let path = resolve_write(target, relative_path).await?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, content).await?;
        Ok(())
    }

    async fn run_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        ensure_writable(target)?;
        let root = tokio::fs::canonicalize(&target.workspace_path).await?;
        if !root.is_dir() {
            return Err(WorkspaceExecutorError::InvalidPath(format!(
                "workspace is not a directory: {}",
                root.display()
            )));
        }
        let mut process = shell_command(command);
        process.current_dir(root).kill_on_drop(true);
        let output = tokio::time::timeout(timeout, process.output())
            .await
            .map_err(|_| WorkspaceExecutorError::Timeout(timeout))??;
        Ok(WorkspaceCommandOutput {
            success: output.status.success(),
            status_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[derive(Debug)]
pub(crate) struct UnavailableExecutor {
    target_id: String,
}

impl UnavailableExecutor {
    pub(crate) fn new(target_id: impl Into<String>) -> Self {
        Self {
            target_id: target_id.into(),
        }
    }

    fn error(&self) -> WorkspaceExecutorError {
        WorkspaceExecutorError::Unavailable(self.target_id.clone())
    }
}

#[async_trait]
impl WorkspaceExecutor for UnavailableExecutor {
    async fn read_file(
        &self,
        _target: &WorkspaceTarget,
        _relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        Err(self.error())
    }

    async fn write_file(
        &self,
        _target: &WorkspaceTarget,
        _relative_path: &str,
        _content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        Err(self.error())
    }

    async fn run_command(
        &self,
        _target: &WorkspaceTarget,
        _command: &str,
        _timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        Err(self.error())
    }
}

fn validate_relative(relative: &str) -> Result<&Path, WorkspaceExecutorError> {
    let path = Path::new(relative);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(WorkspaceExecutorError::InvalidPath(relative.into()));
    }
    Ok(path)
}

async fn resolve_existing(
    target: &WorkspaceTarget,
    relative: &str,
) -> Result<PathBuf, WorkspaceExecutorError> {
    let relative = validate_relative(relative)?;
    let root = tokio::fs::canonicalize(&target.workspace_path).await?;
    let path = tokio::fs::canonicalize(root.join(relative)).await?;
    if !path.starts_with(&root) {
        return Err(WorkspaceExecutorError::InvalidPath(
            relative.display().to_string(),
        ));
    }
    Ok(path)
}

async fn resolve_write(
    target: &WorkspaceTarget,
    relative: &str,
) -> Result<PathBuf, WorkspaceExecutorError> {
    let relative = validate_relative(relative)?;
    let root = tokio::fs::canonicalize(&target.workspace_path).await?;
    let mut cursor = root.clone();
    for component in relative.components() {
        cursor.push(component);
        if tokio::fs::symlink_metadata(&cursor)
            .await
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(WorkspaceExecutorError::InvalidPath(format!(
                "path crosses symbolic link `{}`",
                cursor.display()
            )));
        }
    }
    Ok(root.join(relative))
}

fn ensure_writable(target: &WorkspaceTarget) -> Result<(), WorkspaceExecutorError> {
    if target.read_only {
        Err(WorkspaceExecutorError::ReadOnly(target.id.clone()))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn shell_command(command: &str) -> tokio::process::Command {
    let mut process = tokio::process::Command::new("sh");
    process.args(["-lc", command]);
    process
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;
    use crate::tool::Tool;
    use crate::tool_context::{Cap, ToolContext};
    use crate::tools::{CommandTool, EditTool, ReadTool, WriteTool};

    fn context(target: WorkspaceTarget) -> ToolContext {
        ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
            .with_executor(Arc::new(LocalExecutor), target)
            .with_capability(Cap::Read)
            .with_capability(Cap::Write)
            .with_capability(Cap::Spawn)
    }

    #[derive(Debug, Default)]
    struct RecordingExecutor {
        reads: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl WorkspaceExecutor for RecordingExecutor {
        async fn read_file(
            &self,
            target: &WorkspaceTarget,
            relative_path: &str,
        ) -> Result<Vec<u8>, WorkspaceExecutorError> {
            self.reads
                .lock()
                .unwrap()
                .push((target.id.clone(), relative_path.into()));
            Ok(b"from-mock".to_vec())
        }

        async fn write_file(
            &self,
            _target: &WorkspaceTarget,
            _relative_path: &str,
            _content: &[u8],
        ) -> Result<(), WorkspaceExecutorError> {
            unreachable!("read contract does not write")
        }

        async fn run_command(
            &self,
            _target: &WorkspaceTarget,
            _command: &str,
            _timeout: Duration,
        ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
            unreachable!("read contract does not spawn")
        }
    }

    #[tokio::test]
    async fn tool_uses_injected_executor_and_preserves_target_identity() {
        let executor = Arc::new(RecordingExecutor::default());
        let context = ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
            .with_executor(
                executor.clone(),
                WorkspaceTarget {
                    id: "container:dev".into(),
                    workspace_path: "/workspace".into(),
                    read_only: false,
                },
            )
            .with_capability(Cap::Read);
        let output = ReadTool::new("/must-not-be-used")
            .execute(&context, json!({"file_path":"src/lib.rs"}))
            .await
            .unwrap();
        assert_eq!(output.content, "from-mock");
        assert_eq!(
            *executor.reads.lock().unwrap(),
            [("container:dev".into(), "src/lib.rs".into())]
        );
    }

    #[tokio::test]
    async fn local_executor_contract_covers_read_write_edit_and_command() {
        let workspace = tempfile::tempdir().unwrap();
        let context = context(WorkspaceTarget::local(workspace.path(), false));
        WriteTool::new("/")
            .execute(
                &context,
                json!({"file_path":"value.txt","content":"before"}),
            )
            .await
            .unwrap();
        EditTool::new("/")
            .execute(
                &context,
                json!({"file_path":"value.txt","old_string":"before","new_string":"after"}),
            )
            .await
            .unwrap();
        let read = ReadTool::new("/")
            .execute(&context, json!({"file_path":"value.txt"}))
            .await
            .unwrap();
        assert_eq!(read.content, "after");
        let command = CommandTool::new("/")
            .execute(&context, json!({"command":"printf command-ok"}))
            .await
            .unwrap();
        assert!(command.content.contains("command-ok"));
    }

    #[tokio::test]
    async fn read_only_target_rejects_every_mutating_tool() {
        let workspace = tempfile::tempdir().unwrap();
        tokio::fs::write(workspace.path().join("value.txt"), "before")
            .await
            .unwrap();
        let context = context(WorkspaceTarget::local(workspace.path(), true));
        let write = WriteTool::new("/")
            .execute(&context, json!({"file_path":"new.txt","content":"x"}))
            .await
            .unwrap();
        let edit = EditTool::new("/")
            .execute(
                &context,
                json!({"file_path":"value.txt","old_string":"before","new_string":"after"}),
            )
            .await
            .unwrap();
        let command = CommandTool::new("/")
            .execute(&context, json!({"command":"touch escaped"}))
            .await
            .unwrap();
        assert!(write.is_error && write.content.contains("read-only"));
        assert!(edit.is_error && edit.content.contains("read-only"));
        assert!(command.is_error && command.content.contains("read-only"));
        assert!(!workspace.path().join("new.txt").exists());
        assert!(!workspace.path().join("escaped").exists());
    }

    #[tokio::test]
    async fn unavailable_target_never_falls_back_to_local() {
        let workspace = tempfile::tempdir().unwrap();
        tokio::fs::write(workspace.path().join("value.txt"), "secret")
            .await
            .unwrap();
        let context = ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
            .with_execution_target("ssh:build", workspace.path(), false)
            .with_capability(Cap::Read);
        let output = ReadTool::new(workspace.path())
            .execute(&context, json!({"file_path":"value.txt"}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("ssh:build"));
        assert!(output.content.contains("unavailable"));
    }
}

#[cfg(windows)]
fn shell_command(command: &str) -> tokio::process::Command {
    let mut process = tokio::process::Command::new("cmd");
    process.args(["/C", command]);
    process
}
