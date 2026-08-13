//! macOS-native process sandbox backed by Seatbelt.
//!
//! Structured filesystem operations reuse Runtime's local adapter. Process
//! operations use the fixed system `sandbox-exec` binary and never retry
//! without the generated profile.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use sylvander_agent::workspace_executor::{
    ProcessIsolation, WorkspaceCommandOutput, WorkspaceCommandProgressSink, WorkspaceExecutor,
    WorkspaceExecutorError, WorkspaceListRequest, WorkspaceListResult, WorkspaceReadResult,
    WorkspaceSearchRequest, WorkspaceSearchResult, WorkspaceTarget, validate_command_environment,
};

use super::local::{LocalExecutor, run_local_command_with_profile};

const SEATBELT_EXECUTABLE: &str = "/usr/bin/sandbox-exec";

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MacosSeatbeltExecutor;

impl MacosSeatbeltExecutor {
    pub(crate) fn available() -> bool {
        std::path::Path::new(SEATBELT_EXECUTABLE).is_file()
    }
}

#[async_trait]
impl WorkspaceExecutor for MacosSeatbeltExecutor {
    fn process_isolation(&self) -> ProcessIsolation {
        ProcessIsolation {
            filesystem: true,
            network_denied: true,
            resource_limits: false,
            process_tree: true,
        }
    }

    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        LocalExecutor.read_file(target, relative_path).await
    }

    async fn read_file_bounded(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        max_bytes: usize,
    ) -> Result<WorkspaceReadResult, WorkspaceExecutorError> {
        LocalExecutor
            .read_file_bounded(target, relative_path, max_bytes)
            .await
    }

    async fn write_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        LocalExecutor
            .write_file(target, relative_path, content)
            .await
    }

    async fn run_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        self.run_command_with_environment(target, command, timeout, &BTreeMap::new())
            .await
    }

    async fn run_command_with_environment(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        environment: &BTreeMap<String, String>,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        validate_command_environment(environment)?;
        execute(target, command, timeout, environment, None).await
    }

    async fn run_command_streaming(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        progress: WorkspaceCommandProgressSink,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        self.run_command_streaming_with_environment(
            target,
            command,
            timeout,
            &BTreeMap::new(),
            progress,
        )
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
        validate_command_environment(environment)?;
        execute(target, command, timeout, environment, Some(progress)).await
    }

    async fn run_read_only_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        execute(target, command, timeout, &BTreeMap::new(), None).await
    }

    async fn list(
        &self,
        target: &WorkspaceTarget,
        request: WorkspaceListRequest,
    ) -> Result<WorkspaceListResult, WorkspaceExecutorError> {
        LocalExecutor.list(target, request).await
    }

    async fn search(
        &self,
        target: &WorkspaceTarget,
        request: WorkspaceSearchRequest,
    ) -> Result<WorkspaceSearchResult, WorkspaceExecutorError> {
        LocalExecutor.search(target, request).await
    }
}

async fn execute(
    target: &WorkspaceTarget,
    command: &str,
    timeout: Duration,
    environment: &BTreeMap<String, String>,
    progress: Option<WorkspaceCommandProgressSink>,
) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
    let profile = seatbelt_profile(target).await?;
    run_local_command_with_profile(
        target,
        command,
        timeout,
        environment,
        progress,
        Some(&profile),
    )
    .await
}

async fn seatbelt_profile(target: &WorkspaceTarget) -> Result<String, WorkspaceExecutorError> {
    let root = tokio::fs::canonicalize(&target.workspace_path).await?;
    if !root.is_dir() {
        return Err(WorkspaceExecutorError::InvalidPath(format!(
            "workspace is not a directory: {}",
            root.display()
        )));
    }
    let root = escape_profile_literal(&root.to_string_lossy());
    let write_policy = if target.read_only {
        String::new()
    } else {
        format!("(allow file-write* (subpath \"{root}\"))")
    };
    Ok(format!(
        r#"(version 1)
(deny default)
(allow process*)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow file-read*)
{write_policy}
(allow file-write-data (literal "/dev/null"))
(allow sysctl-read)
(allow mach-lookup)
(allow ipc-posix*)
(allow pseudo-tty)
(deny network*)"#
    ))
}

fn escape_profile_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
#[path = "../../tests/unit/execution_macos_seatbelt.rs"]
mod tests;
