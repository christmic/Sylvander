//! Persistent process authority owned by Runtime execution composition.
//!
//! Ordinary workspace commands are bounded, one-shot operations. Protocol
//! servers and similar workloads instead retain stdin/stdout across many
//! requests. This module defines the neutral port for that second lifetime:
//! callers identify an admitted workload, while a concrete execution target
//! must enforce its filesystem, network, resource, and process-tree policy.
//!
//! The port deliberately contains no MCP vocabulary. MCP is one consumer;
//! language servers and future managed plugin processes may use the same
//! boundary without acquiring protocol-specific dependencies.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

/// Filesystem authority enforced for one persistent process tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PersistentFilesystemAuthority {
    WorkspaceRead,
    WorkspaceWrite,
}

/// Network authority enforced for one persistent process tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PersistentNetworkAuthority {
    Denied,
}

/// Stable, non-secret identity of one admitted persistent workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistentProcessOwner {
    pub(crate) principal_id: String,
    pub(crate) workload_id: String,
    pub(crate) session_id: String,
    pub(crate) policy_revision: u64,
}

/// Hard ceilings applied to the complete child process tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PersistentResourceLimits {
    pub(crate) memory_mb: u32,
    pub(crate) cpu_millis: u32,
    pub(crate) pids: u32,
    pub(crate) max_stdout_frame_bytes: usize,
    pub(crate) max_stderr_bytes: usize,
}

impl Default for PersistentResourceLimits {
    fn default() -> Self {
        Self {
            memory_mb: 1_024,
            cpu_millis: 1_000,
            pids: 256,
            max_stdout_frame_bytes: 16 * 1_024 * 1_024,
            max_stderr_bytes: 64 * 1_024,
        }
    }
}

/// Runtime-created authority for one spawn operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistentProcessAuthority {
    pub(crate) owner: PersistentProcessOwner,
    pub(crate) workspace_root: PathBuf,
    pub(crate) filesystem: PersistentFilesystemAuthority,
    pub(crate) network: PersistentNetworkAuthority,
    pub(crate) resources: PersistentResourceLimits,
    pub(crate) startup_timeout: Duration,
    pub(crate) drain_timeout: Duration,
}

/// Validated executable request passed to a trusted environment adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistentProcessSpec {
    pub(crate) program: String,
    pub(crate) arguments: Vec<String>,
    /// Only Runtime-resolved entries are present. An adapter must clear the
    /// ambient environment before applying this map.
    pub(crate) environment: BTreeMap<String, String>,
}

/// Static isolation truth of one adapter, not a configured claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PersistentProcessIsolation {
    pub(crate) filesystem: bool,
    pub(crate) network_denied: bool,
    pub(crate) resource_limits: bool,
    pub(crate) process_tree: bool,
}

impl PersistentProcessIsolation {
    pub(crate) const fn unavailable() -> Self {
        Self {
            filesystem: false,
            network_denied: false,
            resource_limits: false,
            process_tree: false,
        }
    }

    pub(crate) const fn enforces_required_boundary(self) -> bool {
        self.filesystem && self.network_denied && self.resource_limits && self.process_tree
    }
}

/// Owned bidirectional process returned by an enforcing environment.
#[async_trait]
pub(crate) trait PersistentProcess: Send {
    async fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), PersistentProcessError>;
    async fn read_stdout_frame(&mut self) -> Result<Vec<u8>, PersistentProcessError>;
    async fn close_stdin(&mut self) -> Result<(), PersistentProcessError>;
    async fn wait(&mut self, timeout: Duration) -> Result<(), PersistentProcessError>;
    async fn terminate_tree(&mut self) -> Result<(), PersistentProcessError>;
}

/// Runtime-owned adapter that creates a persistent confined process tree.
#[async_trait]
pub(crate) trait PersistentProcessEnvironment: Send + Sync {
    fn name(&self) -> &str;
    fn isolation(&self) -> PersistentProcessIsolation;

    async fn spawn(
        &self,
        spec: &PersistentProcessSpec,
        authority: &PersistentProcessAuthority,
    ) -> Result<Box<dyn PersistentProcess>, PersistentProcessError>;
}

#[derive(Debug, Error)]
pub(crate) enum PersistentProcessError {
    #[error("persistent process environment `{0}` is unavailable")]
    Unavailable(String),
    #[error("persistent process authority is invalid: {0}")]
    InvalidAuthority(&'static str),
    #[error("persistent process specification is invalid: {0}")]
    InvalidSpecification(&'static str),
    #[error("persistent process I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("persistent process operation timed out after {0:?}")]
    Timeout(Duration),
    #[error("persistent process exited unsuccessfully with status {0:?}")]
    Exited(Option<i32>),
    #[error("persistent process closed its stdout")]
    Closed,
    #[error("persistent process stdout frame exceeds {0} bytes")]
    FrameTooLarge(usize),
}

/// Fail-closed adapter used when no enforcing backend was configured.
#[derive(Debug)]
pub(crate) struct UnavailablePersistentProcessEnvironment {
    name: String,
}

impl UnavailablePersistentProcessEnvironment {
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

#[async_trait]
impl PersistentProcessEnvironment for UnavailablePersistentProcessEnvironment {
    fn name(&self) -> &str {
        &self.name
    }

    fn isolation(&self) -> PersistentProcessIsolation {
        PersistentProcessIsolation::unavailable()
    }

    async fn spawn(
        &self,
        spec: &PersistentProcessSpec,
        authority: &PersistentProcessAuthority,
    ) -> Result<Box<dyn PersistentProcess>, PersistentProcessError> {
        validate_spawn(spec, authority)?;
        Err(PersistentProcessError::Unavailable(self.name.clone()))
    }
}

pub(super) fn validate_spawn(
    spec: &PersistentProcessSpec,
    authority: &PersistentProcessAuthority,
) -> Result<(), PersistentProcessError> {
    if spec.program.trim().is_empty()
        || spec.program.starts_with('-')
        || spec.program.chars().any(char::is_control)
        || spec
            .arguments
            .iter()
            .any(|value| value.chars().any(char::is_control))
        || spec.environment.iter().any(|(name, value)| {
            name.is_empty()
                || name.contains('=')
                || name.chars().any(char::is_control)
                || value.contains('\0')
        })
    {
        return Err(PersistentProcessError::InvalidSpecification(
            "program, arguments, or environment",
        ));
    }
    let owner = &authority.owner;
    if owner.principal_id.trim().is_empty()
        || owner.workload_id.trim().is_empty()
        || owner.session_id.trim().is_empty()
        || owner.policy_revision == 0
        || !authority.workspace_root.is_absolute()
        || authority.startup_timeout.is_zero()
        || authority.drain_timeout.is_zero()
    {
        return Err(PersistentProcessError::InvalidAuthority(
            "owner, workspace, or deadline",
        ));
    }
    let resources = authority.resources;
    if !(128..=65_536).contains(&resources.memory_mb)
        || !(100..=64_000).contains(&resources.cpu_millis)
        || !(16..=32_768).contains(&resources.pids)
        || resources.max_stdout_frame_bytes == 0
        || resources.max_stderr_bytes == 0
    {
        return Err(PersistentProcessError::InvalidAuthority("resource limits"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/execution_persistent.rs"]
mod tests;
