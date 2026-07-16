//! Workspace execution through the system OpenSSH client.
//!
//! Dynamic values are never interpolated into a shell program. The remote
//! program is fixed, paths are passed as positional parameters with POSIX
//! shell quoting, and file contents or user commands travel only on stdin.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use sylvander_agent::workspace_executor::{
    COMMAND_OUTPUT_HEAD_BYTES, MAX_COMMAND_OUTPUT_BYTES_PER_STREAM, WorkspaceCommandOutput,
    WorkspaceCommandProgressSink, WorkspaceCommandStream, WorkspaceEntryKind, WorkspaceExecutor,
    WorkspaceExecutorError, WorkspaceListEntry, WorkspaceListRequest, WorkspaceListResult,
    WorkspaceQueryLimits, WorkspaceReadResult, WorkspaceSearchMatch, WorkspaceSearchRequest,
    WorkspaceSearchResult, WorkspaceTarget,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

const READ_SCRIPT: &str = r#"cd -P "$1" || exit 125
root=$(pwd -P) || exit 125
resolved=$(realpath "./$2" 2>/dev/null) || exit 125
if [ "$root" != / ]; then
  case $resolved in "$root"/*) ;; *) exit 126;; esac
fi
[ -f "$resolved" ] || exit 125
exec cat -- "$resolved""#;
const READ_BOUNDED_SCRIPT: &str = r#"cd -P "$1" || exit 125
root=$(pwd -P) || exit 125
resolved=$(realpath "./$2" 2>/dev/null) || exit 125
if [ "$root" != / ]; then
  case $resolved in "$root"/*) ;; *) exit 126;; esac
fi
[ -f "$resolved" ] || exit 125
total=$(wc -c < "$resolved") || exit 125
printf '%s\0' "$total"
exec head -c "$3" "$resolved""#;
const WRITE_SCRIPT: &str = "cd -P \"$1\" || exit 125\ntarget=$2\ncase $target in */*) mkdir -p -- \"${target%/*}\" || exit 125;; esac\nexec cat > \"$target\"";
const COMMAND_SCRIPT: &str = "cd -P \"$1\" || exit 125\nexec sh -s";
const LIST_SCRIPT: &str = r#"cd -P "$1" || exit 125
root=$(pwd -P) || exit 125
start=./$2
if [ -d "$start" ]; then
  resolved=$(cd -P -- "$start" 2>/dev/null && pwd -P) || exit 125
else
  parent=${start%/*}
  resolved_parent=$(cd -P -- "$parent" 2>/dev/null && pwd -P) || exit 125
  resolved=$resolved_parent/${start##*/}
fi
if [ "$root" != / ]; then
  case $resolved in "$root"|"$root"/*) ;; *) exit 126;; esac
fi
if [ "$3" = 1 ]; then
  find "$start" -name .git -prune -o -mindepth 1 -exec sh -c '
    for path do
      if [ -L "$path" ]; then kind=l; size=0
      elif [ -f "$path" ]; then kind=f; size=$(wc -c < "$path")
      elif [ -d "$path" ]; then kind=d; size=0
      else kind=o; size=0
      fi
      printf "%s\0%s\0%s\0" "$path" "$kind" "$size"
    done
  ' sh {} +
else
  find "$start" -mindepth 1 -maxdepth 1 ! -name .git -exec sh -c '
    for path do
      if [ -L "$path" ]; then kind=l; size=0
      elif [ -f "$path" ]; then kind=f; size=$(wc -c < "$path")
      elif [ -d "$path" ]; then kind=d; size=0
      else kind=o; size=0
      fi
      printf "%s\0%s\0%s\0" "$path" "$kind" "$size"
    done
  ' sh {} +
fi | head -c "$4""#;
const SEARCH_SCRIPT: &str = r#"cd -P "$1" || exit 125
root=$(pwd -P) || exit 125
start=./$2
if [ -d "$start" ]; then
  resolved=$(cd -P -- "$start" 2>/dev/null && pwd -P) || exit 125
else
  parent=${start%/*}
  resolved_parent=$(cd -P -- "$parent" 2>/dev/null && pwd -P) || exit 125
  resolved=$resolved_parent/${start##*/}
fi
if [ "$root" != / ]; then
  case $resolved in "$root"|"$root"/*) ;; *) exit 126;; esac
fi
query=$3
find "$start" -name .git -prune -o -type f -exec sh -c '
  query=$1
  shift
  for path do
    LC_ALL=C grep -n -F -- "$query" "$path" 2>/dev/null | while IFS=: read -r number line; do
      printf "%s\0%s\0%s\0" "$path" "$number" "$line"
    done
  done
' sh "$query" {} + | head -c "$4""#;
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

    fn remote_query_command(
        script: &str,
        target: &WorkspaceTarget,
        arguments: &[String],
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
        for argument in arguments {
            command.push(' ');
            command.push_str(&shell_quote(argument));
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

    async fn invoke_command(
        &self,
        remote_command: OsString,
        stdin: &[u8],
        timeout: Duration,
        progress: Option<WorkspaceCommandProgressSink>,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
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
        let mut child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("SSH command stdin was not piped"))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("SSH command stdout was not piped"))?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("SSH command stderr was not piped"))?;

        let operation = async {
            let write_and_wait = async {
                child_stdin.write_all(stdin).await?;
                child_stdin.shutdown().await?;
                drop(child_stdin);
                child.wait().await
            };
            let (status, stdout, stderr) = tokio::try_join!(
                write_and_wait,
                drain_command_stream(
                    child_stdout,
                    progress
                        .clone()
                        .map(|sink| (WorkspaceCommandStream::Stdout, sink)),
                ),
                drain_command_stream(
                    child_stderr,
                    progress.map(|sink| (WorkspaceCommandStream::Stderr, sink)),
                ),
            )?;
            Ok::<_, io::Error>((status, stdout, stderr))
        };
        let (status, stdout, stderr) = tokio::time::timeout(timeout, operation)
            .await
            .map_err(|_| WorkspaceExecutorError::Timeout(timeout))??;
        Ok(WorkspaceCommandOutput {
            success: status.success(),
            status_code: status.code(),
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            stdout_total_bytes: stdout.total_bytes,
            stderr_total_bytes: stderr.total_bytes,
        })
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

    async fn read_file_bounded(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        max_bytes: usize,
    ) -> Result<WorkspaceReadResult, WorkspaceExecutorError> {
        validate_relative(relative_path)?;
        let transfer_limit = max_bytes.saturating_add(1);
        let remote = Self::remote_query_command(
            READ_BOUNDED_SCRIPT,
            target,
            &[relative_path.to_owned(), transfer_limit.to_string()],
        )?;
        let output = self
            .invoke(remote, &[], Some(self.file_operation_timeout))
            .await?;
        if !output.status.success() {
            return Err(remote_failure("bounded read", &output));
        }
        parse_bounded_read(&output.stdout, max_bytes)
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
        self.invoke_command(remote, command.as_bytes(), timeout, None)
            .await
    }

    async fn run_command_streaming(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        progress: WorkspaceCommandProgressSink,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        ensure_writable(target)?;
        let remote = Self::remote_command(COMMAND_SCRIPT, target, None)?;
        self.invoke_command(remote, command.as_bytes(), timeout, Some(progress))
            .await
    }

    async fn run_read_only_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let remote = Self::remote_command(COMMAND_SCRIPT, target, None)?;
        self.invoke_command(remote, command.as_bytes(), timeout, None)
            .await
    }

    async fn list(
        &self,
        target: &WorkspaceTarget,
        request: WorkspaceListRequest,
    ) -> Result<WorkspaceListResult, WorkspaceExecutorError> {
        validate_relative(&request.relative_path)?;
        let limits = request.limits.bounded()?;
        let cap = limits.max_output_bytes.saturating_add(1);
        let arguments = [
            request.relative_path,
            u8::from(request.recursive).to_string(),
            cap.to_string(),
        ];
        let remote = Self::remote_query_command(LIST_SCRIPT, target, &arguments)?;
        let output = self.invoke(remote, &[], Some(limits.timeout)).await?;
        if !output.status.success() {
            return Err(remote_failure("list", &output));
        }
        Ok(parse_list_output(&output.stdout, limits))
    }

    async fn search(
        &self,
        target: &WorkspaceTarget,
        request: WorkspaceSearchRequest,
    ) -> Result<WorkspaceSearchResult, WorkspaceExecutorError> {
        validate_relative(&request.relative_path)?;
        if request.query.is_empty() || request.query.contains('\0') {
            return Err(WorkspaceExecutorError::InvalidRequest(
                "search query must not be empty or contain NUL".into(),
            ));
        }
        let limits = request.limits.bounded()?;
        let cap = limits.max_output_bytes.saturating_add(1);
        let arguments = [request.relative_path, request.query, cap.to_string()];
        let remote = Self::remote_query_command(SEARCH_SCRIPT, target, &arguments)?;
        let output = self.invoke(remote, &[], Some(limits.timeout)).await?;
        if !output.status.success() {
            return Err(remote_failure("search", &output));
        }
        Ok(parse_search_output(&output.stdout, limits))
    }
}

fn parse_bounded_read(
    output: &[u8],
    max_bytes: usize,
) -> Result<WorkspaceReadResult, WorkspaceExecutorError> {
    let separator = output.iter().position(|byte| *byte == 0).ok_or_else(|| {
        WorkspaceExecutorError::InvalidRequest(
            "remote bounded read returned invalid metadata".into(),
        )
    })?;
    let total_bytes = std::str::from_utf8(&output[..separator])
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or_else(|| {
            WorkspaceExecutorError::InvalidRequest(
                "remote bounded read returned invalid byte count".into(),
            )
        })?;
    let mut bytes = output[separator + 1..].to_vec();
    let observed_bytes = bytes.len() as u64;
    let max_bytes_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    let truncated = total_bytes > max_bytes_u64 || bytes.len() > max_bytes;
    bytes.truncate(max_bytes);
    Ok(WorkspaceReadResult {
        bytes,
        total_bytes: total_bytes.max(observed_bytes),
        truncated,
    })
}

struct BoundedCommandStream {
    bytes: Vec<u8>,
    truncated: bool,
    total_bytes: u64,
}

async fn drain_command_stream(
    mut stream: impl AsyncRead + Unpin,
    progress: Option<(WorkspaceCommandStream, WorkspaceCommandProgressSink)>,
) -> Result<BoundedCommandStream, io::Error> {
    let tail_capacity =
        MAX_COMMAND_OUTPUT_BYTES_PER_STREAM.saturating_sub(COMMAND_OUTPUT_HEAD_BYTES);
    let mut head = Vec::with_capacity(COMMAND_OUTPUT_HEAD_BYTES);
    let mut tail = VecDeque::with_capacity(tail_capacity);
    let mut total_bytes = 0_u64;
    let mut chunk = vec![0_u8; 16 * 1024];
    let mut utf8_pending = Vec::new();
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        if let Some((kind, sink)) = &progress {
            emit_utf8_progress(*kind, sink, &mut utf8_pending, &chunk[..read], false);
        }
        total_bytes = total_bytes.saturating_add(read as u64);
        let mut remaining = &chunk[..read];
        if head.len() < COMMAND_OUTPUT_HEAD_BYTES {
            let head_bytes = remaining
                .len()
                .min(COMMAND_OUTPUT_HEAD_BYTES.saturating_sub(head.len()));
            head.extend_from_slice(&remaining[..head_bytes]);
            remaining = &remaining[head_bytes..];
        }
        for byte in remaining {
            if tail.len() == tail_capacity {
                tail.pop_front();
            }
            if tail_capacity > 0 {
                tail.push_back(*byte);
            }
        }
    }
    if let Some((kind, sink)) = &progress {
        emit_utf8_progress(*kind, sink, &mut utf8_pending, &[], true);
    }

    let mut bytes = head;
    bytes.reserve(tail.len());
    bytes.extend(tail);
    Ok(BoundedCommandStream {
        truncated: total_bytes > bytes.len() as u64,
        total_bytes,
        bytes,
    })
}

fn emit_utf8_progress(
    stream: WorkspaceCommandStream,
    sink: &WorkspaceCommandProgressSink,
    pending: &mut Vec<u8>,
    bytes: &[u8],
    eof: bool,
) {
    pending.extend_from_slice(bytes);
    let mut offset = 0;
    while offset < pending.len() {
        match std::str::from_utf8(&pending[offset..]) {
            Ok(text) => {
                sink.emit(stream, text.to_owned());
                offset = pending.len();
            }
            Err(error) => {
                let valid_end = offset + error.valid_up_to();
                if valid_end > offset {
                    sink.emit(
                        stream,
                        String::from_utf8_lossy(&pending[offset..valid_end]).into_owned(),
                    );
                }
                if let Some(invalid_len) = error.error_len() {
                    sink.emit(stream, "\u{fffd}".into());
                    offset = valid_end.saturating_add(invalid_len);
                } else {
                    offset = valid_end;
                    break;
                }
            }
        }
    }
    if offset > 0 {
        pending.drain(..offset);
    }
    if eof && !pending.is_empty() {
        sink.emit(stream, String::from_utf8_lossy(pending).into_owned());
        pending.clear();
    }
}

fn parse_list_output(raw: &[u8], limits: WorkspaceQueryLimits) -> WorkspaceListResult {
    let truncated_by_bytes = raw.len() > limits.max_output_bytes;
    let bounded = &raw[..raw.len().min(limits.max_output_bytes)];
    let fields: Vec<_> = bounded.split(|byte| *byte == 0).collect();
    let mut entries = Vec::new();
    let mut index = 0;
    let mut truncated = truncated_by_bytes;
    while index + 2 < fields.len() && entries.len() < limits.max_results {
        let path = String::from_utf8_lossy(fields[index]);
        let kind = match fields[index + 1] {
            b"f" => WorkspaceEntryKind::File,
            b"d" => WorkspaceEntryKind::Directory,
            b"l" => WorkspaceEntryKind::Symlink,
            _ => WorkspaceEntryKind::Other,
        };
        let size = String::from_utf8_lossy(fields[index + 2])
            .parse()
            .unwrap_or(0);
        let relative_path = path.strip_prefix("./").unwrap_or(&path);
        if relative_path.chars().count() > limits.max_line_chars {
            truncated = true;
            break;
        }
        entries.push(WorkspaceListEntry {
            relative_path: relative_path.into(),
            kind,
            size,
        });
        index += 3;
    }
    if index + 2 < fields.len() || (!bounded.is_empty() && !bounded.ends_with(b"\0")) {
        truncated = true;
    }
    WorkspaceListResult { entries, truncated }
}

fn parse_search_output(raw: &[u8], limits: WorkspaceQueryLimits) -> WorkspaceSearchResult {
    let truncated_by_bytes = raw.len() > limits.max_output_bytes;
    let bounded = &raw[..raw.len().min(limits.max_output_bytes)];
    let fields: Vec<_> = bounded.split(|byte| *byte == 0).collect();
    let mut matches = Vec::new();
    let mut index = 0;
    let mut truncated = truncated_by_bytes;
    while index + 2 < fields.len() && matches.len() < limits.max_results {
        let path = String::from_utf8_lossy(fields[index]);
        let relative_path = path.strip_prefix("./").unwrap_or(&path);
        let line_number = String::from_utf8_lossy(fields[index + 1])
            .parse()
            .unwrap_or(0);
        let line = truncate_chars(
            &String::from_utf8_lossy(fields[index + 2]),
            limits.max_line_chars,
        );
        matches.push(WorkspaceSearchMatch {
            relative_path: relative_path.into(),
            line_number,
            line,
        });
        index += 3;
    }
    if index + 2 < fields.len() || (!bounded.is_empty() && !bounded.ends_with(b"\0")) {
        truncated = true;
    }
    WorkspaceSearchResult { matches, truncated }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut characters = value.chars();
    let mut result: String = characters.by_ref().take(max_chars).collect();
    if characters.next().is_some() {
        result.push('…');
    }
    result
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
    use std::sync::{Arc, Mutex};

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
        assert!(argv.contains("exec cat -- \"$resolved\""));
        assert!(argv.contains("'/srv/工作区/it'\\''s safe'"));
        assert!(argv.contains("'文档/计划.md'"));
    }

    #[tokio::test]
    async fn bounded_read_transfers_only_the_limit_probe_and_reports_total_bytes() {
        let fake = FakeSsh::new("printf '12\\0hello world!'");
        let result = fake
            .executor()
            .read_file_bounded(&target(false), "文档/计划.md", 5)
            .await
            .expect("bounded read succeeds");
        assert_eq!(result.bytes, b"hello");
        assert_eq!(result.total_bytes, 12);
        assert!(result.truncated);

        let argv = fs::read_to_string(&fake.argv_log).expect("argv log");
        assert!(argv.contains("exec head -c \"$3\" \"$resolved\""));
        assert!(argv.contains("'文档/计划.md'"));
        assert!(argv.contains("'6'"));
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
        assert!(!output.stdout_truncated);
        assert!(!output.stderr_truncated);
        assert_eq!(output.stdout_total_bytes, 3);
        assert_eq!(output.stderr_total_bytes, 3);
        assert_eq!(
            fs::read_to_string(&fake.stdin_log).expect("stdin log"),
            "printf '%s' \"用户命令; $(safe as data)\""
        );
    }

    #[tokio::test]
    async fn command_concurrently_drains_and_bounds_both_output_streams() {
        const BODY_BYTES: usize = 300_000;
        const STDOUT_HEAD: &str = "stdout-head\n";
        const STDOUT_TAIL: &str = "\nstdout-tail";
        const STDERR_HEAD: &str = "stderr-head\n";
        const STDERR_TAIL: &str = "\nstderr-tail";
        let fake = FakeSsh::new(&format!(
            "printf '{STDOUT_HEAD}'\n\
             awk 'BEGIN {{ for (i = 0; i < {BODY_BYTES}; i++) printf \"o\" }}'\n\
             printf '{STDOUT_TAIL}'\n\
             printf '{STDERR_HEAD}' >&2\n\
             awk 'BEGIN {{ for (i = 0; i < {BODY_BYTES}; i++) printf \"e\" }}' >&2\n\
             printf '{STDERR_TAIL}' >&2"
        ));
        let output = fake
            .executor()
            .run_command(&target(false), "true", Duration::from_secs(5))
            .await
            .expect("large dual-stream command completes");

        assert!(output.success);
        assert!(output.stdout_truncated);
        assert!(output.stderr_truncated);
        assert_eq!(output.stdout.len(), MAX_COMMAND_OUTPUT_BYTES_PER_STREAM);
        assert_eq!(output.stderr.len(), MAX_COMMAND_OUTPUT_BYTES_PER_STREAM);
        assert_eq!(
            output.stdout_total_bytes,
            (STDOUT_HEAD.len() + BODY_BYTES + STDOUT_TAIL.len()) as u64
        );
        assert_eq!(
            output.stderr_total_bytes,
            (STDERR_HEAD.len() + BODY_BYTES + STDERR_TAIL.len()) as u64
        );
        assert!(output.stdout.starts_with(STDOUT_HEAD.as_bytes()));
        assert!(output.stdout.ends_with(STDOUT_TAIL.as_bytes()));
        assert!(output.stderr.starts_with(STDERR_HEAD.as_bytes()));
        assert!(output.stderr.ends_with(STDERR_TAIL.as_bytes()));
    }

    #[tokio::test]
    async fn command_streaming_preserves_unicode_and_stream_identity() {
        let fake = FakeSsh::new("printf '远程输出'; printf '远程错误' >&2");
        let deltas = Arc::new(Mutex::new(Vec::new()));
        let captured = deltas.clone();
        let progress = WorkspaceCommandProgressSink::new(move |stream, delta| {
            captured.lock().unwrap().push((stream, delta));
        });

        let output = fake
            .executor()
            .run_command_streaming(
                &target(false),
                "printf ignored",
                Duration::from_secs(5),
                progress,
            )
            .await
            .expect("streaming command completes");

        assert!(output.success);
        let deltas = deltas.lock().unwrap();
        assert!(
            deltas
                .iter()
                .any(|(stream, delta)| *stream == WorkspaceCommandStream::Stdout
                    && delta.contains("远程输出"))
        );
        assert!(
            deltas
                .iter()
                .any(|(stream, delta)| *stream == WorkspaceCommandStream::Stderr
                    && delta.contains("远程错误"))
        );
    }

    #[tokio::test]
    async fn dropping_command_future_terminates_the_ssh_transport() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let survived = directory.path().join("survived");
        let fake = FakeSsh::new(&format!(
            "printf ready > {}; sleep 1; printf survived > {}",
            shell_quote(ready.to_str().unwrap()),
            shell_quote(survived.to_str().unwrap())
        ));
        let executor = fake.executor();
        let task = tokio::spawn(async move {
            executor
                .run_command(&target(false), "ignored", Duration::from_secs(10))
                .await
        });
        for _ in 0..500 {
            if ready.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            ready.exists(),
            "SSH transport never reached its ready boundary"
        );

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert!(
            !survived.exists(),
            "cancelled SSH transport continued after its future was dropped"
        );
    }

    #[tokio::test]
    async fn structured_list_and_search_parse_bounded_unicode_results() {
        let list_fake = FakeSsh::new(
            "printf '%s\\0%s\\0%s\\0%s\\0%s\\0%s\\0' './src' d 0 './src/螃蟹.rs' f 12",
        );
        let list = list_fake
            .executor()
            .list(
                &target(true),
                WorkspaceListRequest {
                    relative_path: ".".into(),
                    recursive: true,
                    limits: WorkspaceQueryLimits::default(),
                },
            )
            .await
            .expect("list succeeds on a read-only target");
        assert_eq!(list.entries.len(), 2);
        assert_eq!(list.entries[0].kind, WorkspaceEntryKind::Directory);
        assert_eq!(list.entries[1].relative_path, "src/螃蟹.rs");
        assert_eq!(list.entries[1].size, 12);
        assert!(!list.truncated);

        let search_fake = FakeSsh::new(
            "printf '%s\\0%s\\0%s\\0%s\\0%s\\0%s\\0' './src/螃蟹.rs' 7 '匹配：可靠伙伴' './src/螃蟹.rs' 9 '匹配：再次'",
        );
        let search = search_fake
            .executor()
            .search(
                &target(true),
                WorkspaceSearchRequest {
                    relative_path: "src".into(),
                    query: "匹配".into(),
                    limits: WorkspaceQueryLimits {
                        max_results: 1,
                        max_line_chars: 5,
                        ..WorkspaceQueryLimits::default()
                    },
                },
            )
            .await
            .expect("search succeeds on a read-only target");
        assert_eq!(search.matches.len(), 1);
        assert_eq!(search.matches[0].line_number, 7);
        assert_eq!(search.matches[0].line, "匹配：可靠…");
        assert!(search.truncated);
        let argv = fs::read_to_string(&search_fake.argv_log).expect("argv log");
        assert!(argv.contains("'src'"));
        assert!(argv.contains("'匹配'"));
    }

    #[cfg(unix)]
    #[test]
    fn structured_queries_reject_symlink_paths_outside_the_workspace() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("secret.txt"), "outside-secret\n").expect("outside file");
        symlink(outside.path(), workspace.path().join("escape")).expect("workspace symlink");

        let list = std::process::Command::new("sh")
            .args([
                "-c",
                LIST_SCRIPT,
                "--",
                workspace.path().to_str().expect("UTF-8 workspace"),
                "escape",
                "1",
                "4096",
            ])
            .output()
            .expect("run list script");
        assert_eq!(list.status.code(), Some(126));
        assert!(list.stdout.is_empty());

        let search = std::process::Command::new("sh")
            .args([
                "-c",
                SEARCH_SCRIPT,
                "--",
                workspace.path().to_str().expect("UTF-8 workspace"),
                "escape/secret.txt",
                "outside-secret",
                "4096",
            ])
            .output()
            .expect("run search script");
        assert_eq!(search.status.code(), Some(126));
        assert!(search.stdout.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn reads_reject_external_symlinks_and_bound_large_files_remotely() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("secret.txt"), "outside-secret\n").expect("outside file");
        symlink(outside.path(), workspace.path().join("escape")).expect("workspace symlink");

        for script in [READ_SCRIPT, READ_BOUNDED_SCRIPT] {
            let mut arguments = vec![
                "-c",
                script,
                "--",
                workspace.path().to_str().expect("UTF-8 workspace"),
                "escape/secret.txt",
            ];
            if script == READ_BOUNDED_SCRIPT {
                arguments.push("17");
            }
            let output = std::process::Command::new("sh")
                .args(arguments)
                .output()
                .expect("run read script");
            assert_eq!(output.status.code(), Some(126));
            assert!(output.stdout.is_empty());
        }

        let content = vec![b'x'; 64 * 1024];
        fs::write(workspace.path().join("large.bin"), &content).expect("large file");
        let output = std::process::Command::new("sh")
            .args([
                "-c",
                READ_BOUNDED_SCRIPT,
                "--",
                workspace.path().to_str().expect("UTF-8 workspace"),
                "large.bin",
                "65",
            ])
            .output()
            .expect("run bounded read script");
        assert!(output.status.success());
        let separator = output
            .stdout
            .iter()
            .position(|byte| *byte == 0)
            .expect("metadata separator");
        assert_eq!(
            std::str::from_utf8(&output.stdout[..separator])
                .expect("UTF-8 size")
                .trim(),
            "65536"
        );
        assert_eq!(output.stdout.len() - separator - 1, 65);
        let parsed = parse_bounded_read(&output.stdout, 64).expect("parse bounded read");
        assert_eq!(parsed.bytes, vec![b'x'; 64]);
        assert_eq!(parsed.total_bytes, 65_536);
        assert!(parsed.truncated);
    }

    #[tokio::test]
    async fn read_only_command_uses_ssh_while_mutating_command_is_rejected() {
        let fake = FakeSsh::new("cat > \"$0.stdin\"\nprintf inspected");
        let executor = fake.executor();
        let output = executor
            .run_read_only_command(&target(true), "git status --short", Duration::from_secs(5))
            .await
            .expect("trusted read-only command runs");
        assert_eq!(output.stdout, b"inspected");
        assert_eq!(
            fs::read_to_string(&fake.stdin_log).expect("stdin log"),
            "git status --short"
        );
        assert!(matches!(
            executor
                .run_command(&target(true), "git clean -fd", Duration::from_secs(5))
                .await,
            Err(WorkspaceExecutorError::ReadOnly(_))
        ));
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

    #[tokio::test]
    async fn structured_query_timeout_terminates_the_ssh_process() {
        let fake = FakeSsh::new("exec sleep 5");
        let timeout = Duration::from_millis(30);
        let result = fake
            .executor()
            .list(
                &target(true),
                WorkspaceListRequest {
                    relative_path: ".".into(),
                    recursive: false,
                    limits: WorkspaceQueryLimits {
                        timeout,
                        ..WorkspaceQueryLimits::default()
                    },
                },
            )
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
