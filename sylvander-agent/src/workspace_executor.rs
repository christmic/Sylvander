//! Location-neutral workspace operations used by coding tools.
//!
//! This module defines Agent-owned values, routing, bounds, and the
//! [`WorkspaceExecutor`] port. It deliberately contains no host filesystem or
//! process adapter: Runtime selects and injects a concrete target for each
//! turn. An unresolved target uses [`UnavailableExecutor`] and fails closed.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

#[cfg(test)]
use crate::tool::ToolTestExt as _;

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
    #[error("invalid workspace request: {0}")]
    InvalidRequest(String),
    #[error("workspace operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("workspace command timed out after {0:?}")]
    Timeout(Duration),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceReadResult {
    pub bytes: Vec<u8>,
    pub total_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceCommandOutput {
    pub success: bool,
    pub status_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_total_bytes: u64,
    pub stderr_total_bytes: u64,
}

pub const MAX_COMMAND_OUTPUT_BYTES_PER_STREAM: usize = 256 * 1024;
pub const COMMAND_OUTPUT_HEAD_BYTES: usize = 64 * 1024;
const MAX_COMMAND_ENVIRONMENT_ENTRIES: usize = 64;
const MAX_COMMAND_ENVIRONMENT_VALUE_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceCommandStream {
    Stdout,
    Stderr,
}

#[derive(Clone)]
pub struct WorkspaceCommandProgressSink {
    emit_delta: Arc<dyn Fn(WorkspaceCommandStream, String) + Send + Sync>,
}

impl Debug for WorkspaceCommandProgressSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceCommandProgressSink")
            .finish_non_exhaustive()
    }
}

impl WorkspaceCommandProgressSink {
    #[must_use]
    pub fn new(
        emit_delta: impl Fn(WorkspaceCommandStream, String) + Send + Sync + 'static,
    ) -> Self {
        Self {
            emit_delta: Arc::new(emit_delta),
        }
    }

    pub fn emit(&self, stream: WorkspaceCommandStream, delta: String) {
        if !delta.is_empty() {
            (self.emit_delta)(stream, delta);
        }
    }
}
pub const MAX_QUERY_RESULTS: usize = 1_000;
pub const MAX_QUERY_LINE_CHARS: usize = 4_096;
pub const MAX_QUERY_OUTPUT_BYTES: usize = 1024 * 1024;
pub const MAX_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceQueryLimits {
    pub max_results: usize,
    pub max_line_chars: usize,
    pub max_output_bytes: usize,
    pub timeout: Duration,
}

impl Default for WorkspaceQueryLimits {
    fn default() -> Self {
        Self {
            max_results: 200,
            max_line_chars: 1_000,
            max_output_bytes: 256 * 1024,
            timeout: Duration::from_secs(10),
        }
    }
}

impl WorkspaceQueryLimits {
    pub fn bounded(self) -> Result<Self, WorkspaceExecutorError> {
        if self.max_results == 0
            || self.max_line_chars == 0
            || self.max_output_bytes == 0
            || self.timeout.is_zero()
        {
            return Err(WorkspaceExecutorError::InvalidRequest(
                "query limits must be greater than zero".into(),
            ));
        }
        Ok(Self {
            max_results: self.max_results.min(MAX_QUERY_RESULTS),
            max_line_chars: self.max_line_chars.min(MAX_QUERY_LINE_CHARS),
            max_output_bytes: self.max_output_bytes.min(MAX_QUERY_OUTPUT_BYTES),
            timeout: self.timeout.min(MAX_QUERY_TIMEOUT),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceListRequest {
    pub relative_path: String,
    pub recursive: bool,
    pub limits: WorkspaceQueryLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceListEntry {
    pub relative_path: String,
    pub kind: WorkspaceEntryKind,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceListResult {
    pub entries: Vec<WorkspaceListEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSearchRequest {
    pub relative_path: String,
    pub query: String,
    pub limits: WorkspaceQueryLimits,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSearchMatch {
    pub relative_path: String,
    pub line_number: u64,
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSearchResult {
    pub matches: Vec<WorkspaceSearchMatch>,
    pub truncated: bool,
}

/// OS controls enforced for process trees launched by an executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIsolation {
    pub filesystem: bool,
    pub network_denied: bool,
    pub resource_limits: bool,
}

impl ProcessIsolation {
    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            filesystem: false,
            network_denied: false,
            resource_limits: false,
        }
    }

    #[must_use]
    pub const fn restricted() -> Self {
        Self {
            filesystem: true,
            network_denied: true,
            resource_limits: true,
        }
    }

    #[must_use]
    pub const fn enforces_sandbox(self) -> bool {
        self.filesystem && self.network_denied && self.resource_limits
    }
}

/// Transport-neutral operations needed by the built-in coding tools.
///
/// Operation futures are cancellation boundaries. Implementations that spawn a
/// process or transport must terminate it when the returned future is dropped,
/// so interrupting an Agent turn does not leave the command running detached.
#[async_trait]
pub trait WorkspaceExecutor: Send + Sync + Debug {
    /// Describe process isolation enforced by this concrete executor.
    ///
    /// The default is deliberately unconfined. Executors must opt in only
    /// when the operating system or container runtime enforces every claim.
    fn process_isolation(&self) -> ProcessIsolation {
        ProcessIsolation::unavailable()
    }

    fn select_mount_target(
        &self,
        target: &WorkspaceTarget,
        reference: Option<&str>,
    ) -> Result<WorkspaceTarget, WorkspaceExecutorError> {
        if let Some(reference) = reference {
            return Err(WorkspaceExecutorError::InvalidRequest(format!(
                "workspace mount `@{reference}` is unavailable"
            )));
        }
        Ok(target.clone())
    }

    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError>;

    async fn read_file_bounded(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        max_bytes: usize,
    ) -> Result<WorkspaceReadResult, WorkspaceExecutorError> {
        let mut bytes = self.read_file(target, relative_path).await?;
        let total_bytes = bytes.len() as u64;
        let truncated = bytes.len() > max_bytes;
        bytes.truncate(max_bytes);
        Ok(WorkspaceReadResult {
            bytes,
            total_bytes,
            truncated,
        })
    }

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

    async fn run_command_with_environment(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        environment: &BTreeMap<String, String>,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        validate_command_environment(environment)?;
        if environment.is_empty() {
            self.run_command(target, command, timeout).await
        } else {
            Err(WorkspaceExecutorError::InvalidRequest(format!(
                "execution target `{}` does not support command environment overrides",
                target.id
            )))
        }
    }

    async fn run_command_streaming(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        _progress: WorkspaceCommandProgressSink,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        self.run_command(target, command, timeout).await
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
        if environment.is_empty() {
            self.run_command_streaming(target, command, timeout, progress)
                .await
        } else {
            Err(WorkspaceExecutorError::InvalidRequest(format!(
                "execution target `{}` does not support command environment overrides",
                target.id
            )))
        }
    }

    /// Run a command selected by a trusted structured read-only operation.
    ///
    /// Implementations deliberately do not apply the target's `read_only`
    /// guard. Callers must never expose this primitive as an arbitrary shell
    /// tool.
    async fn run_read_only_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let _ = (command, timeout);
        Err(WorkspaceExecutorError::Unavailable(target.id.clone()))
    }

    async fn list(
        &self,
        target: &WorkspaceTarget,
        _request: WorkspaceListRequest,
    ) -> Result<WorkspaceListResult, WorkspaceExecutorError> {
        Err(WorkspaceExecutorError::Unavailable(target.id.clone()))
    }

    async fn search(
        &self,
        target: &WorkspaceTarget,
        _request: WorkspaceSearchRequest,
    ) -> Result<WorkspaceSearchResult, WorkspaceExecutorError> {
        Err(WorkspaceExecutorError::Unavailable(target.id.clone()))
    }
}

/// Agent-domain capability surface for one logical workspace mount.
///
/// Runtime derives this value from authenticated product configuration. It is
/// intentionally independent of the public API DTO so workspace routing can
/// be embedded and tested without a service-protocol dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceCapabilities {
    pub read: bool,
    pub write: bool,
    pub command: bool,
    pub git: bool,
}

impl Default for WorkspaceCapabilities {
    fn default() -> Self {
        Self {
            read: true,
            write: false,
            command: false,
            git: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MountedWorkspace {
    pub executor: Arc<dyn WorkspaceExecutor>,
    pub target: WorkspaceTarget,
    pub capabilities: WorkspaceCapabilities,
}

/// Routes logical `@reference/path` requests to role-bearing workspaces while
/// preserving the ordinary task workspace as the unqualified default.
#[derive(Debug, Clone)]
pub struct WorkspaceRouter {
    default_reference: String,
    mounts: BTreeMap<String, MountedWorkspace>,
}

impl WorkspaceRouter {
    pub fn new(
        default_reference: impl Into<String>,
        mounts: impl IntoIterator<Item = (String, MountedWorkspace)>,
    ) -> Result<Self, WorkspaceExecutorError> {
        let default_reference = default_reference.into();
        let mounts = mounts.into_iter().collect::<BTreeMap<_, _>>();
        if !mounts.contains_key(&default_reference) {
            return Err(WorkspaceExecutorError::InvalidRequest(format!(
                "default workspace mount `{default_reference}` is unavailable"
            )));
        }
        Ok(Self {
            default_reference,
            mounts,
        })
    }

    fn route_path<'a>(
        &'a self,
        relative_path: &str,
    ) -> Result<(String, &'a MountedWorkspace, String, bool), WorkspaceExecutorError> {
        let (reference, path, explicit) = if let Some(logical) = relative_path.strip_prefix('@') {
            let (reference, path) = logical.split_once('/').unwrap_or((logical, "."));
            (reference, path, true)
        } else {
            (self.default_reference.as_str(), relative_path, false)
        };
        if reference.is_empty() || path.starts_with('@') {
            return Err(WorkspaceExecutorError::InvalidPath(relative_path.into()));
        }
        let mount = self.mounts.get(reference).ok_or_else(|| {
            WorkspaceExecutorError::InvalidRequest(format!(
                "workspace mount `@{reference}` is unavailable"
            ))
        })?;
        Ok((reference.to_owned(), mount, path.to_owned(), explicit))
    }

    fn route_target<'a>(
        &'a self,
        target: &WorkspaceTarget,
    ) -> Result<&'a MountedWorkspace, WorkspaceExecutorError> {
        let reference = target
            .workspace_path
            .to_str()
            .and_then(|path| path.strip_prefix('@'))
            .unwrap_or(&self.default_reference);
        self.mounts.get(reference).ok_or_else(|| {
            WorkspaceExecutorError::InvalidRequest(format!(
                "workspace mount `@{reference}` is unavailable"
            ))
        })
    }

    fn require(
        mount: &MountedWorkspace,
        allowed: bool,
        operation: &str,
    ) -> Result<(), WorkspaceExecutorError> {
        if allowed {
            Ok(())
        } else {
            Err(WorkspaceExecutorError::InvalidRequest(format!(
                "{operation} is disabled for workspace mount `{}`",
                mount.target.workspace_path.display()
            )))
        }
    }
}

#[async_trait]
impl WorkspaceExecutor for WorkspaceRouter {
    fn select_mount_target(
        &self,
        _target: &WorkspaceTarget,
        reference: Option<&str>,
    ) -> Result<WorkspaceTarget, WorkspaceExecutorError> {
        let reference = reference.unwrap_or(&self.default_reference);
        let mount = self.mounts.get(reference).ok_or_else(|| {
            WorkspaceExecutorError::InvalidRequest(format!(
                "workspace mount `@{reference}` is unavailable"
            ))
        })?;
        Ok(WorkspaceTarget {
            id: "workspace-router".into(),
            workspace_path: format!("@{reference}").into(),
            read_only: mount.target.read_only,
        })
    }

    async fn read_file(
        &self,
        _target: &WorkspaceTarget,
        relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        let (_, mount, path, _) = self.route_path(relative_path)?;
        Self::require(mount, mount.capabilities.read, "read")?;
        mount.executor.read_file(&mount.target, &path).await
    }

    async fn read_file_bounded(
        &self,
        _target: &WorkspaceTarget,
        relative_path: &str,
        max_bytes: usize,
    ) -> Result<WorkspaceReadResult, WorkspaceExecutorError> {
        let (_, mount, path, _) = self.route_path(relative_path)?;
        Self::require(mount, mount.capabilities.read, "read")?;
        mount
            .executor
            .read_file_bounded(&mount.target, &path, max_bytes)
            .await
    }

    async fn write_file(
        &self,
        _target: &WorkspaceTarget,
        relative_path: &str,
        content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        let (_, mount, path, _) = self.route_path(relative_path)?;
        Self::require(mount, mount.capabilities.write, "write")?;
        mount
            .executor
            .write_file(&mount.target, &path, content)
            .await
    }

    async fn run_command(
        &self,
        _target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let mount = self.route_target(_target)?;
        Self::require(mount, mount.capabilities.command, "command")?;
        mount
            .executor
            .run_command(&mount.target, command, timeout)
            .await
    }

    async fn run_command_with_environment(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        environment: &BTreeMap<String, String>,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let mount = self.route_target(target)?;
        Self::require(mount, mount.capabilities.command, "command")?;
        mount
            .executor
            .run_command_with_environment(&mount.target, command, timeout, environment)
            .await
    }

    async fn run_command_streaming(
        &self,
        _target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        progress: WorkspaceCommandProgressSink,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let mount = self.route_target(_target)?;
        Self::require(mount, mount.capabilities.command, "command")?;
        mount
            .executor
            .run_command_streaming(&mount.target, command, timeout, progress)
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
        let mount = self.route_target(target)?;
        Self::require(mount, mount.capabilities.command, "command")?;
        mount
            .executor
            .run_command_streaming_with_environment(
                &mount.target,
                command,
                timeout,
                environment,
                progress,
            )
            .await
    }

    async fn run_read_only_command(
        &self,
        _target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let mount = self.route_target(_target)?;
        Self::require(mount, mount.capabilities.git, "git")?;
        mount
            .executor
            .run_read_only_command(&mount.target, command, timeout)
            .await
    }

    async fn list(
        &self,
        _target: &WorkspaceTarget,
        mut request: WorkspaceListRequest,
    ) -> Result<WorkspaceListResult, WorkspaceExecutorError> {
        let (reference, mount, path, explicit) = self.route_path(&request.relative_path)?;
        Self::require(mount, mount.capabilities.read, "list")?;
        request.relative_path = path;
        let mut result = mount.executor.list(&mount.target, request).await?;
        if explicit {
            for entry in &mut result.entries {
                entry.relative_path = format!("@{reference}/{}", entry.relative_path);
            }
        }
        Ok(result)
    }

    async fn search(
        &self,
        _target: &WorkspaceTarget,
        mut request: WorkspaceSearchRequest,
    ) -> Result<WorkspaceSearchResult, WorkspaceExecutorError> {
        let (reference, mount, path, explicit) = self.route_path(&request.relative_path)?;
        Self::require(mount, mount.capabilities.read, "search")?;
        request.relative_path = path;
        let mut result = mount.executor.search(&mount.target, request).await?;
        if explicit {
            for found in &mut result.matches {
                found.relative_path = format!("@{reference}/{}", found.relative_path);
            }
        }
        Ok(result)
    }
}

/// Fail-closed executor used when Runtime did not bind a workspace target.
#[derive(Debug)]
pub struct UnavailableExecutor {
    target_id: String,
}

impl UnavailableExecutor {
    /// Create a sentinel that reports the unresolved target identifier.
    pub fn new(target_id: impl Into<String>) -> Self {
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

    async fn run_read_only_command(
        &self,
        _target: &WorkspaceTarget,
        _command: &str,
        _timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        Err(self.error())
    }
}

/// Validate bounded environment overrides before an executor launches work.
///
/// This is Agent-owned prepared-call policy. Every concrete Runtime executor
/// must apply it before passing environment values to a process launcher.
pub fn validate_command_environment(
    environment: &BTreeMap<String, String>,
) -> Result<(), WorkspaceExecutorError> {
    if environment.len() > MAX_COMMAND_ENVIRONMENT_ENTRIES {
        return Err(WorkspaceExecutorError::InvalidRequest(
            "command environment has too many entries".into(),
        ));
    }
    if environment.iter().any(|(name, value)| {
        name.is_empty()
            || name.len() > 128
            || !name.bytes().enumerate().all(|(index, byte)| {
                byte == b'_'
                    || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
            })
            || value.len() > MAX_COMMAND_ENVIRONMENT_VALUE_BYTES
            || value.contains('\0')
    }) {
        return Err(WorkspaceExecutorError::InvalidRequest(
            "command environment contains an invalid name or value".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/workspace_executor.rs"]
mod tests;
