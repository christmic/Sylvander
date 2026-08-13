//! OCI-backed persistent process environment.
//!
//! One container is one governed process tree. The adapter never invokes a
//! shell for the workload, never inherits Runtime's environment, and never
//! grants host access when an isolation control cannot be expressed.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::persistent::{
    PersistentFilesystemAuthority, PersistentProcess, PersistentProcessAuthority,
    PersistentProcessEnvironment, PersistentProcessError, PersistentProcessIsolation,
    PersistentProcessSpec, validate_spawn,
};

/// Enforcing persistent-process adapter for Docker/Podman-compatible CLIs.
#[derive(Clone)]
pub(crate) struct ContainerPersistentProcessEnvironment {
    name: String,
    executable: PathBuf,
    image: String,
}

impl fmt::Debug for ContainerPersistentProcessEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContainerPersistentProcessEnvironment")
            .field("name", &self.name)
            .field("executable", &self.executable)
            .field("image", &self.image)
            .finish()
    }
}

impl ContainerPersistentProcessEnvironment {
    pub(crate) fn new(
        name: impl Into<String>,
        executable: impl Into<PathBuf>,
        image: impl Into<String>,
    ) -> Result<Self, PersistentProcessError> {
        let name = name.into();
        let executable = executable.into();
        let image = image.into();
        let executable_text = executable.to_string_lossy();
        if name.trim().is_empty() {
            return Err(PersistentProcessError::InvalidSpecification(
                "execution environment name",
            ));
        }
        if executable.as_os_str().is_empty()
            || !executable.is_absolute()
            || executable_text.starts_with('-')
            || executable_text.chars().any(char::is_control)
        {
            return Err(PersistentProcessError::InvalidSpecification(
                "container runtime executable",
            ));
        }
        if image.is_empty()
            || image.trim() != image
            || image.starts_with('-')
            || image
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(PersistentProcessError::InvalidSpecification(
                "container image",
            ));
        }
        Ok(Self {
            name,
            executable,
            image,
        })
    }
}

struct ContainerPersistentProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr_drain: JoinHandle<()>,
    max_stdout_frame_bytes: usize,
    cleanup: ContainerCleanup,
}

struct ContainerCleanup {
    executable: PathBuf,
    container_name: String,
    armed: bool,
}

impl ContainerCleanup {
    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn remove(&mut self) -> Result<(), PersistentProcessError> {
        if !self.armed {
            return Ok(());
        }
        let status = Command::new(&self.executable)
            .arg("rm")
            .arg("-f")
            .arg(&self.container_name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_clear()
            .kill_on_drop(true)
            .status()
            .await?;
        if status.success() {
            self.disarm();
            Ok(())
        } else {
            Err(PersistentProcessError::Exited(status.code()))
        }
    }
}

impl Drop for ContainerCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let executable = self.executable.clone();
        let container_name = self.container_name.clone();
        std::thread::spawn(move || {
            let _ = std::process::Command::new(executable)
                .arg("rm")
                .arg("-f")
                .arg(container_name)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .env_clear()
                .status();
        });
    }
}

#[async_trait]
impl PersistentProcessEnvironment for ContainerPersistentProcessEnvironment {
    fn name(&self) -> &str {
        &self.name
    }

    fn isolation(&self) -> PersistentProcessIsolation {
        PersistentProcessIsolation {
            filesystem: true,
            network_denied: true,
            resource_limits: true,
            process_tree: true,
        }
    }

    async fn spawn(
        &self,
        spec: &PersistentProcessSpec,
        authority: &PersistentProcessAuthority,
    ) -> Result<Box<dyn PersistentProcess>, PersistentProcessError> {
        validate_spawn(spec, authority)?;
        let workspace = tokio::fs::canonicalize(&authority.workspace_root).await?;
        if !workspace.is_dir() {
            return Err(PersistentProcessError::InvalidAuthority(
                "workspace is not a directory",
            ));
        }
        let workspace = workspace
            .to_str()
            .filter(|value| !value.contains(','))
            .ok_or(PersistentProcessError::InvalidAuthority("workspace path"))?;
        let container_name = format!("sylvander-persistent-{}", Uuid::new_v4());
        let arguments =
            container_arguments(&self.image, spec, authority, workspace, &container_name);
        let mut command = Command::new(&self.executable);
        command
            .args(arguments)
            .env_clear()
            .envs(&spec.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or(PersistentProcessError::InvalidSpecification("piped stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(PersistentProcessError::InvalidSpecification("piped stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(PersistentProcessError::InvalidSpecification("piped stderr"))?;
        let max_stderr_bytes = authority.resources.max_stderr_bytes;
        let stderr_drain = tokio::spawn(async move {
            let mut bounded = Vec::with_capacity(max_stderr_bytes.min(8_192));
            let mut limited = stderr.take(max_stderr_bytes as u64);
            let _ = limited.read_to_end(&mut bounded).await;
            let mut stderr = limited.into_inner();
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        });
        Ok(Box::new(ContainerPersistentProcess {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stderr_drain,
            max_stdout_frame_bytes: authority.resources.max_stdout_frame_bytes,
            cleanup: ContainerCleanup {
                executable: self.executable.clone(),
                container_name,
                armed: true,
            },
        }))
    }
}

#[async_trait]
impl PersistentProcess for ContainerPersistentProcess {
    async fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), PersistentProcessError> {
        let stdin = self.stdin.as_mut().ok_or(PersistentProcessError::Closed)?;
        stdin.write_all(bytes).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn read_stdout_frame(&mut self) -> Result<Vec<u8>, PersistentProcessError> {
        let mut frame = Vec::new();
        loop {
            let available = self.stdout.fill_buf().await?;
            if available.is_empty() {
                return Err(PersistentProcessError::Closed);
            }
            let consumed = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |position| position + 1);
            if frame.len().saturating_add(consumed) > self.max_stdout_frame_bytes {
                return Err(PersistentProcessError::FrameTooLarge(
                    self.max_stdout_frame_bytes,
                ));
            }
            let complete = available[consumed - 1] == b'\n';
            frame.extend_from_slice(&available[..consumed]);
            self.stdout.consume(consumed);
            if complete {
                return Ok(frame);
            }
        }
    }

    async fn close_stdin(&mut self) -> Result<(), PersistentProcessError> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin.shutdown().await?;
        }
        Ok(())
    }

    async fn wait(&mut self, duration: Duration) -> Result<(), PersistentProcessError> {
        let status = tokio::time::timeout(duration, self.child.wait())
            .await
            .map_err(|_| PersistentProcessError::Timeout(duration))??;
        self.cleanup.disarm();
        self.stderr_drain.abort();
        if status.success() {
            Ok(())
        } else {
            Err(PersistentProcessError::Exited(status.code()))
        }
    }

    async fn terminate_tree(&mut self) -> Result<(), PersistentProcessError> {
        self.stdin.take();
        self.cleanup.remove().await?;
        let _ = self.child.wait().await;
        self.stderr_drain.abort();
        Ok(())
    }
}

fn container_arguments(
    image: &str,
    spec: &PersistentProcessSpec,
    authority: &PersistentProcessAuthority,
    workspace: &str,
    container_name: &str,
) -> Vec<OsString> {
    let mut mount = format!("type=bind,source={workspace},target=/workspace");
    if authority.filesystem == PersistentFilesystemAuthority::WorkspaceRead {
        mount.push_str(",readonly");
    }
    let cpu = format!(
        "{}.{:03}",
        authority.resources.cpu_millis / 1_000,
        authority.resources.cpu_millis % 1_000
    );
    let mut arguments = [
        "run".into(),
        "--rm".into(),
        "--name".into(),
        container_name.into(),
        "--network=none".into(),
        "--memory".into(),
        format!("{}m", authority.resources.memory_mb).into(),
        "--cpus".into(),
        cpu.into(),
        "--pids-limit".into(),
        authority.resources.pids.to_string().into(),
        "--read-only".into(),
        "--tmpfs".into(),
        "/tmp:rw,nosuid,nodev,size=64m".into(),
        "--security-opt".into(),
        "no-new-privileges".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--interactive".into(),
        "--mount".into(),
        mount.into(),
        "--workdir".into(),
        "/workspace".into(),
    ]
    .into_iter()
    .collect::<Vec<_>>();
    for name in spec.environment.keys() {
        arguments.extend(["--env".into(), name.into()]);
    }
    arguments.push(image.into());
    arguments.push(spec.program.clone().into());
    arguments.extend(spec.arguments.iter().map(OsString::from));
    arguments
}

#[cfg(test)]
#[path = "../../tests/unit/execution_persistent_container.rs"]
mod tests;
