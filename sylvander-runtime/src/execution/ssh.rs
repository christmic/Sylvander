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
const COMMAND_SCRIPT: &str = r#"cd -P "$1" || exit 125
command -v setsid >/dev/null 2>&1 || {
  printf '%s\n' 'remote host requires setsid for cancellable commands' >&2
  exit 127
}
umask 077
program=${TMPDIR:-/tmp}/sylvander-command-$$
cat > "$program" || exit 125
child=
cleanup() {
  trap - EXIT HUP INT TERM
  if [ -n "$child" ]; then
    kill -TERM -- "-$child" 2>/dev/null || true
    sleep 1
    kill -KILL -- "-$child" 2>/dev/null || true
  fi
  rm -f -- "$program"
}
trap cleanup EXIT HUP INT TERM
setsid sh "$program" &
child=$!
wait "$child"
status=$?
child=
rm -f -- "$program"
trap - EXIT HUP INT TERM
exit "$status""#;
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
    known_hosts_path: PathBuf,
    control_path: PathBuf,
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
            .field("known_hosts_path", &self.known_hosts_path)
            .field("control_path", &self.control_path)
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
        known_hosts_path: impl Into<PathBuf>,
        control_path: impl Into<PathBuf>,
    ) -> Result<Self, WorkspaceExecutorError> {
        Self::with_executable(
            "ssh",
            host,
            port,
            user,
            identity_path,
            known_hosts_path,
            control_path,
        )
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
        known_hosts_path: impl Into<PathBuf>,
        control_path: impl Into<PathBuf>,
    ) -> Result<Self, WorkspaceExecutorError> {
        let executable = executable.into();
        let host = host.into();
        let user = user.into();
        let identity_path = identity_path.into();
        let known_hosts_path = known_hosts_path.into();
        let control_path = control_path.into();
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
        if !known_hosts_path.is_absolute() {
            return Err(invalid("SSH known-hosts path must be absolute"));
        }
        if !control_path.is_absolute() {
            return Err(invalid("SSH control path must be absolute"));
        }
        Ok(Self {
            executable,
            host,
            port,
            user,
            identity_path,
            known_hosts_path,
            control_path,
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

    fn configure_command(&self, command: &mut Command) {
        command
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("StrictHostKeyChecking=yes")
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile={}",
                self.known_hosts_path.display()
            ))
            .arg("-o")
            .arg("ControlMaster=auto")
            .arg("-o")
            .arg("ControlPersist=60")
            .arg("-o")
            .arg(format!("ControlPath={}", self.control_path.display()))
            .arg("-o")
            .arg("ServerAliveInterval=15")
            .arg("-o")
            .arg("ServerAliveCountMax=2")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-p")
            .arg(self.port.to_string())
            .arg("-i")
            .arg(&self.identity_path);
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
        self.configure_command(&mut command);
        command
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
        self.configure_command(&mut command);
        command
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
#[path = "../../tests/unit/execution_ssh.rs"]
mod tests;
