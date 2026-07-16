//! Workspace execution through the system OpenSSH client.
//!
//! Dynamic values are never interpolated into a shell program. The remote
//! program is fixed, paths are passed as positional parameters with POSIX
//! shell quoting, and file contents or user commands travel only on stdin.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use sylvander_agent::workspace_executor::{
    WorkspaceCommandOutput, WorkspaceExecutor, WorkspaceExecutorError, WorkspaceTarget,
};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const READ_SCRIPT: &str = "cd -P \"$1\" || exit 125\nexec cat -- \"$2\"";
const WRITE_SCRIPT: &str = "cd -P \"$1\" || exit 125\ntarget=$2\ncase $target in */*) mkdir -p -- \"${target%/*}\" || exit 125;; esac\nexec cat > \"$target\"";
const COMMAND_SCRIPT: &str = "cd -P \"$1\" || exit 125\nexec sh -s";
const DEFAULT_FILE_OPERATION_TIMEOUT: Duration = Duration::from_mins(2);

/// A workspace executor backed by the system `ssh` program.
///
/// Construction validates the remote endpoint so it cannot be interpreted as
/// an OpenSSH option. The identity path is passed directly as an argv item.
#[derive(Clone)]
pub struct SshExecutor {
    executable: PathBuf,
    host: String,
    port: u16,
    user: String,
    identity_path: PathBuf,
    file_operation_timeout: Duration,
}

impl fmt::Debug for SshExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SshExecutor")
            .field("executable", &self.executable)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("identity_path", &"[REDACTED]")
            .field("file_operation_timeout", &self.file_operation_timeout)
            .finish()
    }
}

impl SshExecutor {
    /// Create an executor that invokes the system `ssh` executable.
    pub fn new(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        identity_path: impl Into<PathBuf>,
    ) -> Result<Self, WorkspaceExecutorError> {
        Self::with_executable("ssh", host, port, user, identity_path)
    }

    /// Create an executor with a specific OpenSSH-compatible executable.
    ///
    /// This is useful for hermetic tests and packaged OpenSSH clients.
    pub fn with_executable(
        executable: impl Into<PathBuf>,
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        identity_path: impl Into<PathBuf>,
    ) -> Result<Self, WorkspaceExecutorError> {
        let executable = executable.into();
        let host = host.into();
        let user = user.into();
        let identity_path = identity_path.into();
        validate_endpoint("SSH host", &host, |character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | ':')
        })?;
        validate_endpoint("SSH user", &user, |character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })?;
        if port == 0 {
            return Err(invalid("SSH port must be greater than zero"));
        }
        if executable.as_os_str().is_empty() {
            return Err(invalid("SSH executable must not be empty"));
        }
        if !identity_path.is_absolute() {
            return Err(invalid("SSH identity path must be absolute"));
        }
        Ok(Self {
            executable,
            host,
            port,
            user,
            identity_path,
            file_operation_timeout: DEFAULT_FILE_OPERATION_TIMEOUT,
        })
    }

    /// Override the deadline used by remote reads and writes.
    #[must_use]
    pub fn with_file_operation_timeout(mut self, timeout: Duration) -> Self {
        self.file_operation_timeout = timeout;
        self
    }

    fn destination(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    fn remote_command(
        script: &str,
        target: &WorkspaceTarget,
        relative: Option<&str>,
    ) -> Result<OsString, WorkspaceExecutorError> {
        let workspace = target
            .workspace_path
            .to_str()
            .ok_or_else(|| invalid("remote workspace path must be valid UTF-8"))?;
        if !target.workspace_path.is_absolute() || workspace.contains('\0') {
            return Err(invalid(format!(
                "remote workspace must be an absolute path: {}",
                target.workspace_path.display()
            )));
        }

        let mut command = format!(
            "sh -c {} -- {}",
            shell_quote(script),
            shell_quote(workspace)
        );
        if let Some(relative) = relative {
            command.push(' ');
            command.push_str(&shell_quote(relative));
        }
        Ok(command.into())
    }

    async fn invoke(
        &self,
        remote_command: OsString,
        stdin: &[u8],
        timeout: Option<Duration>,
    ) -> Result<std::process::Output, WorkspaceExecutorError> {
        let mut command = Command::new(&self.executable);
        command
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-p")
            .arg(self.port.to_string())
            .arg("-i")
            .arg(&self.identity_path)
            .arg(self.destination())
            .arg(remote_command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command.spawn()?;
        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin.write_all(stdin).await?;
            child_stdin.shutdown().await?;
        }
        let output = match timeout {
            Some(duration) => tokio::time::timeout(duration, child.wait_with_output())
                .await
                .map_err(|_| WorkspaceExecutorError::Timeout(duration))??,
            None => child.wait_with_output().await?,
        };
        Ok(output)
    }

    async fn file_operation(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        script: &str,
        stdin: &[u8],
    ) -> Result<std::process::Output, WorkspaceExecutorError> {
        validate_relative(relative_path)?;
        let remote = Self::remote_command(script, target, Some(relative_path))?;
        let output = self
            .invoke(remote, stdin, Some(self.file_operation_timeout))
            .await?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(remote_failure("file operation", &output))
        }
    }
}

#[async_trait]
impl WorkspaceExecutor for SshExecutor {
    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        let output = self
            .file_operation(target, relative_path, READ_SCRIPT, &[])
            .await?;
        Ok(output.stdout)
    }

    async fn write_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        ensure_writable(target)?;
        self.file_operation(target, relative_path, WRITE_SCRIPT, content)
            .await?;
        Ok(())
    }

    async fn run_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        ensure_writable(target)?;
        let remote = Self::remote_command(COMMAND_SCRIPT, target, None)?;
        let output = self
            .invoke(remote, command.as_bytes(), Some(timeout))
            .await?;
        Ok(WorkspaceCommandOutput {
            success: output.status.success(),
            status_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

fn validate_endpoint(
    label: &str,
    value: &str,
    allowed: impl Fn(char) -> bool,
) -> Result<(), WorkspaceExecutorError> {
    if value.is_empty() || value.starts_with('-') || !value.chars().all(allowed) {
        return Err(invalid(format!("invalid {label}: {value:?}")));
    }
    Ok(())
}

fn validate_relative(relative: &str) -> Result<(), WorkspaceExecutorError> {
    let path = Path::new(relative);
    if relative.contains('\0')
        || path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid(relative));
    }
    Ok(())
}

fn ensure_writable(target: &WorkspaceTarget) -> Result<(), WorkspaceExecutorError> {
    if target.read_only {
        Err(WorkspaceExecutorError::ReadOnly(target.id.clone()))
    } else {
        Ok(())
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn invalid(message: impl Into<String>) -> WorkspaceExecutorError {
    WorkspaceExecutorError::InvalidPath(message.into())
}

fn remote_failure(operation: &str, output: &std::process::Output) -> WorkspaceExecutorError {
    let status = output
        .status
        .code()
        .map_or_else(|| "signal".into(), |code| code.to_string());
    let stderr = String::from_utf8_lossy(&output.stderr);
    WorkspaceExecutorError::Io(io::Error::other(format!(
        "remote {operation} exited with {status}: {}",
        stderr.trim()
    )))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    struct FakeSsh {
        _directory: TempDir,
        executable: PathBuf,
        argv_log: PathBuf,
        stdin_log: PathBuf,
    }

    impl FakeSsh {
        fn new(body: &str) -> Self {
            let directory = tempfile::tempdir().expect("tempdir");
            let executable = directory.path().join("fake-ssh");
            let argv_log = directory.path().join("argv");
            let stdin_log = directory.path().join("fake-ssh.stdin");
            let script = format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n{}",
                shell_quote(argv_log.to_str().expect("UTF-8 temp path")),
                body
            );
            fs::write(&executable, script).expect("write fake ssh");
            let mut permissions = fs::metadata(&executable).expect("metadata").permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&executable, permissions).expect("chmod");
            Self {
                _directory: directory,
                executable,
                argv_log,
                stdin_log,
            }
        }

        fn executor(&self) -> SshExecutor {
            SshExecutor::with_executable(
                &self.executable,
                "dev.example",
                2222,
                "agent-user",
                "/keys/id test",
            )
            .expect("valid executor")
        }
    }

    fn target(read_only: bool) -> WorkspaceTarget {
        WorkspaceTarget {
            id: "remote-dev".into(),
            workspace_path: PathBuf::from("/srv/工作区/it's safe"),
            read_only,
        }
    }

    #[tokio::test]
    async fn read_uses_fixed_batch_argv_and_preserves_unicode() {
        let fake = FakeSsh::new("printf '你好，Sylvander'");
        let bytes = fake
            .executor()
            .read_file(&target(false), "文档/计划.md")
            .await
            .expect("read succeeds");
        assert_eq!(String::from_utf8(bytes).expect("UTF-8"), "你好，Sylvander");

        let argv = fs::read_to_string(&fake.argv_log).expect("argv log");
        assert!(argv.contains("-o\nBatchMode=yes\n-p\n2222\n-i\n/keys/id test"));
        assert!(argv.contains("agent-user@dev.example"));
        assert!(argv.contains("exec cat -- \"$2\""));
        assert!(argv.contains("'/srv/工作区/it'\\''s safe'"));
        assert!(argv.contains("'文档/计划.md'"));
    }

    #[tokio::test]
    async fn write_sends_content_only_on_stdin() {
        let fake = FakeSsh::new("cat > \"$0.stdin\"");
        let content = "line one\n'$(touch should-not-run)'\n中文".as_bytes();
        fake.executor()
            .write_file(&target(false), "src/输入.txt", content)
            .await
            .expect("write succeeds");
        assert_eq!(fs::read(&fake.stdin_log).expect("stdin log"), content);
        let argv = fs::read_to_string(&fake.argv_log).expect("argv log");
        assert!(!argv.contains("touch should-not-run"));
    }

    #[tokio::test]
    async fn command_reports_status_and_streams_program_on_stdin() {
        let fake = FakeSsh::new("cat > \"$0.stdin\"\nprintf out\nprintf err >&2\nexit 7");
        let output = fake
            .executor()
            .run_command(
                &target(false),
                "printf '%s' \"用户命令; $(safe as data)\"",
                Duration::from_secs(5),
            )
            .await
            .expect("command completes");
        assert!(!output.success);
        assert_eq!(output.status_code, Some(7));
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
        assert_eq!(
            fs::read_to_string(&fake.stdin_log).expect("stdin log"),
            "printf '%s' \"用户命令; $(safe as data)\""
        );
    }

    #[tokio::test]
    async fn read_only_rejects_mutations_before_spawning_ssh() {
        let fake = FakeSsh::new("exit 99");
        let executor = fake.executor();
        assert!(matches!(
            executor.write_file(&target(true), "file", b"x").await,
            Err(WorkspaceExecutorError::ReadOnly(id)) if id == "remote-dev"
        ));
        assert!(matches!(
            executor
                .run_command(&target(true), "true", Duration::from_secs(1))
                .await,
            Err(WorkspaceExecutorError::ReadOnly(id)) if id == "remote-dev"
        ));
        assert!(!fake.argv_log.exists());
    }

    #[tokio::test]
    async fn command_timeout_terminates_the_ssh_process() {
        let fake = FakeSsh::new("exec sleep 5");
        let timeout = Duration::from_millis(30);
        let result = fake
            .executor()
            .run_command(&target(false), "true", timeout)
            .await;
        assert!(matches!(result, Err(WorkspaceExecutorError::Timeout(value)) if value == timeout));
    }

    #[tokio::test]
    async fn file_operation_timeout_terminates_the_ssh_process() {
        let fake = FakeSsh::new("exec sleep 5");
        let timeout = Duration::from_millis(30);
        let result = fake
            .executor()
            .with_file_operation_timeout(timeout)
            .read_file(&target(false), "slow.txt")
            .await;
        assert!(matches!(result, Err(WorkspaceExecutorError::Timeout(value)) if value == timeout));
    }

    #[test]
    fn endpoint_and_relative_path_validation_reject_option_and_traversal_inputs() {
        assert!(SshExecutor::new("-oProxyCommand=bad", 22, "agent", "/key").is_err());
        assert!(SshExecutor::new("host", 22, "user;bad", "/key").is_err());
        assert!(validate_relative("../secret").is_err());
        assert!(validate_relative("/absolute").is_err());
    }
}
