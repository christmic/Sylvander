//! Workspace execution through an OCI-compatible container runtime.
//!
//! Every operation starts a disposable container with the selected workspace
//! bind-mounted at `/workspace`. Dynamic values remain separate argv items;
//! only the fixed operation script is interpreted by the container shell.

use std::collections::VecDeque;
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
use uuid::Uuid;

const READ_SCRIPT: &str = r#"root=$(pwd -P) || exit 125
resolved=$(realpath "./$1" 2>/dev/null) || exit 125
case $resolved in "$root"/*) ;; *) exit 126;; esac
[ -f "$resolved" ] || exit 125
exec cat -- "$resolved""#;
const READ_BOUNDED_SCRIPT: &str = r#"root=$(pwd -P) || exit 125
resolved=$(realpath "./$1" 2>/dev/null) || exit 125
case $resolved in "$root"/*) ;; *) exit 126;; esac
[ -f "$resolved" ] || exit 125
total=$(wc -c < "$resolved") || exit 125
printf '%s\0' "$total"
exec head -c "$2" "$resolved""#;
const WRITE_SCRIPT: &str = r#"target=$1
case $target in */*) mkdir -p -- "${target%/*}" || exit 125;; esac
exec cat > "$target""#;
const COMMAND_SCRIPT: &str = "exec sh -s";
const LIST_SCRIPT: &str = r#"root=$(pwd -P) || exit 125
start=./$1
if [ -d "$start" ]; then
  resolved=$(cd -P -- "$start" 2>/dev/null && pwd -P) || exit 125
else
  parent=${start%/*}
  resolved_parent=$(cd -P -- "$parent" 2>/dev/null && pwd -P) || exit 125
  resolved=$resolved_parent/${start##*/}
fi
case $resolved in "$root"|"$root"/*) ;; *) exit 126;; esac
if [ "$2" = 1 ]; then
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
fi | head -c "$3""#;
const SEARCH_SCRIPT: &str = r#"root=$(pwd -P) || exit 125
start=./$1
if [ -d "$start" ]; then
  resolved=$(cd -P -- "$start" 2>/dev/null && pwd -P) || exit 125
else
  parent=${start%/*}
  resolved_parent=$(cd -P -- "$parent" 2>/dev/null && pwd -P) || exit 125
  resolved=$resolved_parent/${start##*/}
fi
case $resolved in "$root"|"$root"/*) ;; *) exit 126;; esac
query=$2
find "$start" -name .git -prune -o -type f -exec sh -c '
  query=$1
  shift
  for path do
    LC_ALL=C grep -n -F -- "$query" "$path" 2>/dev/null | while IFS=: read -r number line; do
      printf "%s\0%s\0%s\0" "$path" "$number" "$line"
    done
  done
' sh "$query" {} + | head -c "$3""#;
const DEFAULT_FILE_OPERATION_TIMEOUT: Duration = Duration::from_mins(2);

#[derive(Clone)]
pub struct ContainerExecutor {
    executable: PathBuf,
    image: String,
    file_operation_timeout: Duration,
    resources: ContainerResourcePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerResourcePolicy {
    pub memory_mb: u32,
    pub cpu_millis: u32,
    pub pids_limit: u32,
}

impl Default for ContainerResourcePolicy {
    fn default() -> Self {
        Self {
            memory_mb: 2_048,
            cpu_millis: 2_000,
            pids_limit: 512,
        }
    }
}

struct CheckedInvocation<'a> {
    operation: &'a str,
    script: &'a str,
    arguments: &'a [String],
    stdin: &'a [u8],
    timeout: Duration,
    read_only_mount: bool,
}

struct ContainerProcess {
    command: Command,
    cleanup: ContainerCleanup,
}

struct ContainerCleanup {
    executable: PathBuf,
    name: String,
    armed: bool,
}

impl ContainerCleanup {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ContainerCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let executable = self.executable.clone();
        let name = self.name.clone();
        std::thread::spawn(move || {
            let _ = std::process::Command::new(executable)
                .arg("rm")
                .arg("-f")
                .arg(name)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }
}

impl fmt::Debug for ContainerExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContainerExecutor")
            .field("executable", &self.executable)
            .field("image", &self.image)
            .field("file_operation_timeout", &self.file_operation_timeout)
            .field("resources", &self.resources)
            .finish()
    }
}

impl ContainerExecutor {
    pub fn new(
        executable: impl Into<PathBuf>,
        image: impl Into<String>,
    ) -> Result<Self, WorkspaceExecutorError> {
        let executable = executable.into();
        let image = image.into();
        let executable_text = executable.to_string_lossy();
        if executable.as_os_str().is_empty()
            || executable_text.starts_with('-')
            || executable_text.chars().any(char::is_control)
        {
            return Err(invalid("invalid container runtime executable"));
        }
        if image.is_empty()
            || image.trim() != image
            || image.starts_with('-')
            || image
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(invalid("invalid container image"));
        }
        Ok(Self {
            executable,
            image,
            file_operation_timeout: DEFAULT_FILE_OPERATION_TIMEOUT,
            resources: ContainerResourcePolicy::default(),
        })
    }

    #[must_use]
    pub fn with_file_operation_timeout(mut self, timeout: Duration) -> Self {
        self.file_operation_timeout = timeout;
        self
    }

    pub fn with_resource_policy(
        mut self,
        resources: ContainerResourcePolicy,
    ) -> Result<Self, WorkspaceExecutorError> {
        if !(128..=65_536).contains(&resources.memory_mb)
            || !(100..=64_000).contains(&resources.cpu_millis)
            || !(16..=32_768).contains(&resources.pids_limit)
        {
            return Err(invalid("invalid container resource policy"));
        }
        self.resources = resources;
        Ok(self)
    }

    async fn process(
        &self,
        target: &WorkspaceTarget,
        script: &str,
        arguments: &[String],
        read_only_mount: bool,
    ) -> Result<ContainerProcess, WorkspaceExecutorError> {
        let workspace = tokio::fs::canonicalize(&target.workspace_path).await?;
        if !workspace.is_dir() {
            return Err(invalid(format!(
                "container workspace is not a directory: {}",
                workspace.display()
            )));
        }
        let workspace = workspace
            .to_str()
            .ok_or_else(|| invalid("container workspace path must be valid UTF-8"))?;
        if workspace.contains(',') {
            return Err(invalid("container workspace path must not contain a comma"));
        }
        let mut mount = format!("type=bind,source={workspace},target=/workspace");
        if read_only_mount {
            mount.push_str(",readonly");
        }
        let name = format!("sylvander-{}", Uuid::new_v4());
        let cpus = format!(
            "{}.{:03}",
            self.resources.cpu_millis / 1_000,
            self.resources.cpu_millis % 1_000
        );
        let mut command = Command::new(&self.executable);
        command
            .arg("run")
            .arg("--rm")
            .arg("--name")
            .arg(&name)
            .arg("--network=none")
            .arg("--memory")
            .arg(format!("{}m", self.resources.memory_mb))
            .arg("--cpus")
            .arg(cpus)
            .arg("--pids-limit")
            .arg(self.resources.pids_limit.to_string())
            .arg("--read-only")
            .arg("--tmpfs")
            .arg("/tmp:rw,nosuid,nodev,size=64m")
            .arg("--security-opt")
            .arg("no-new-privileges")
            .arg("--cap-drop")
            .arg("ALL")
            .arg("--interactive")
            .arg("--mount")
            .arg(mount)
            .arg("--workdir")
            .arg("/workspace")
            .arg(&self.image)
            .arg("sh")
            .arg("-c")
            .arg(script)
            .arg("--")
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Ok(ContainerProcess {
            command,
            cleanup: ContainerCleanup {
                executable: self.executable.clone(),
                name,
                armed: true,
            },
        })
    }

    async fn invoke(
        &self,
        target: &WorkspaceTarget,
        script: &str,
        arguments: &[String],
        stdin: &[u8],
        timeout: Duration,
        read_only_mount: bool,
    ) -> Result<std::process::Output, WorkspaceExecutorError> {
        let mut process = self
            .process(target, script, arguments, read_only_mount)
            .await?;
        let mut child = process.command.spawn()?;
        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin.write_all(stdin).await?;
            child_stdin.shutdown().await?;
        }
        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                process.cleanup.disarm();
                Ok(output)
            }
            Ok(Err(error)) => Err(error.into()),
            Err(_) => Err(WorkspaceExecutorError::Timeout(timeout)),
        }
    }

    async fn invoke_command(
        &self,
        target: &WorkspaceTarget,
        script: &str,
        stdin: &[u8],
        timeout: Duration,
        progress: Option<WorkspaceCommandProgressSink>,
        read_only_mount: bool,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        let mut process = self.process(target, script, &[], read_only_mount).await?;
        let mut child = process.command.spawn()?;
        let mut child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("container stdin was not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("container stdout was not piped"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("container stderr was not piped"))?;
        let execution = async {
            let write_and_wait = async {
                child_stdin.write_all(stdin).await?;
                child_stdin.shutdown().await?;
                drop(child_stdin);
                child.wait().await
            };
            let (status, stdout, stderr) = tokio::try_join!(
                write_and_wait,
                capture_stream(
                    stdout,
                    progress
                        .clone()
                        .map(|sink| (WorkspaceCommandStream::Stdout, sink)),
                ),
                capture_stream(
                    stderr,
                    progress.map(|sink| (WorkspaceCommandStream::Stderr, sink)),
                ),
            )?;
            Ok::<_, io::Error>((status, stdout, stderr))
        };
        let (status, stdout, stderr) = match tokio::time::timeout(timeout, execution).await {
            Ok(Ok(result)) => {
                process.cleanup.disarm();
                result
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => return Err(WorkspaceExecutorError::Timeout(timeout)),
        };
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

    async fn checked_invoke(
        &self,
        target: &WorkspaceTarget,
        request: CheckedInvocation<'_>,
    ) -> Result<std::process::Output, WorkspaceExecutorError> {
        let output = self
            .invoke(
                target,
                request.script,
                request.arguments,
                request.stdin,
                request.timeout,
                request.read_only_mount,
            )
            .await?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(container_failure(request.operation, &output))
        }
    }
}

#[async_trait]
impl WorkspaceExecutor for ContainerExecutor {
    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        validate_relative(relative_path)?;
        Ok(self
            .checked_invoke(
                target,
                CheckedInvocation {
                    operation: "read",
                    script: READ_SCRIPT,
                    arguments: &[relative_path.into()],
                    stdin: &[],
                    timeout: self.file_operation_timeout,
                    read_only_mount: true,
                },
            )
            .await?
            .stdout)
    }

    async fn read_file_bounded(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        max_bytes: usize,
    ) -> Result<WorkspaceReadResult, WorkspaceExecutorError> {
        validate_relative(relative_path)?;
        let output = self
            .checked_invoke(
                target,
                CheckedInvocation {
                    operation: "bounded read",
                    script: READ_BOUNDED_SCRIPT,
                    arguments: &[
                        relative_path.into(),
                        max_bytes.saturating_add(1).to_string(),
                    ],
                    stdin: &[],
                    timeout: self.file_operation_timeout,
                    read_only_mount: true,
                },
            )
            .await?;
        parse_bounded_read(&output.stdout, max_bytes)
    }

    async fn write_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        ensure_writable(target)?;
        validate_relative(relative_path)?;
        self.checked_invoke(
            target,
            CheckedInvocation {
                operation: "write",
                script: WRITE_SCRIPT,
                arguments: &[relative_path.into()],
                stdin: content,
                timeout: self.file_operation_timeout,
                read_only_mount: false,
            },
        )
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
        self.invoke_command(
            target,
            COMMAND_SCRIPT,
            command.as_bytes(),
            timeout,
            None,
            false,
        )
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
        self.invoke_command(
            target,
            COMMAND_SCRIPT,
            command.as_bytes(),
            timeout,
            Some(progress),
            false,
        )
        .await
    }

    async fn run_read_only_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        self.invoke_command(
            target,
            COMMAND_SCRIPT,
            command.as_bytes(),
            timeout,
            None,
            true,
        )
        .await
    }

    async fn list(
        &self,
        target: &WorkspaceTarget,
        request: WorkspaceListRequest,
    ) -> Result<WorkspaceListResult, WorkspaceExecutorError> {
        validate_relative(&request.relative_path)?;
        let limits = request.limits.bounded()?;
        let output = self
            .checked_invoke(
                target,
                CheckedInvocation {
                    operation: "list",
                    script: LIST_SCRIPT,
                    arguments: &[
                        request.relative_path,
                        u8::from(request.recursive).to_string(),
                        limits.max_output_bytes.saturating_add(1).to_string(),
                    ],
                    stdin: &[],
                    timeout: limits.timeout,
                    read_only_mount: true,
                },
            )
            .await?;
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
        let output = self
            .checked_invoke(
                target,
                CheckedInvocation {
                    operation: "search",
                    script: SEARCH_SCRIPT,
                    arguments: &[
                        request.relative_path,
                        request.query,
                        limits.max_output_bytes.saturating_add(1).to_string(),
                    ],
                    stdin: &[],
                    timeout: limits.timeout,
                    read_only_mount: true,
                },
            )
            .await?;
        Ok(parse_search_output(&output.stdout, limits))
    }
}

struct CapturedStream {
    bytes: Vec<u8>,
    truncated: bool,
    total_bytes: u64,
}

async fn capture_stream(
    mut stream: impl AsyncRead + Unpin,
    progress: Option<(WorkspaceCommandStream, WorkspaceCommandProgressSink)>,
) -> io::Result<CapturedStream> {
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
            let count = remaining
                .len()
                .min(COMMAND_OUTPUT_HEAD_BYTES.saturating_sub(head.len()));
            head.extend_from_slice(&remaining[..count]);
            remaining = &remaining[count..];
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
    bytes.extend(tail);
    Ok(CapturedStream {
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

fn parse_bounded_read(
    output: &[u8],
    max_bytes: usize,
) -> Result<WorkspaceReadResult, WorkspaceExecutorError> {
    let separator = output
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| invalid("container bounded read returned invalid metadata"))?;
    let total_bytes = std::str::from_utf8(&output[..separator])
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or_else(|| invalid("container bounded read returned invalid byte count"))?;
    let mut bytes = output[separator + 1..].to_vec();
    let observed_bytes = bytes.len() as u64;
    let truncated =
        total_bytes > u64::try_from(max_bytes).unwrap_or(u64::MAX) || bytes.len() > max_bytes;
    bytes.truncate(max_bytes);
    Ok(WorkspaceReadResult {
        bytes,
        total_bytes: total_bytes.max(observed_bytes),
        truncated,
    })
}

fn parse_list_output(raw: &[u8], limits: WorkspaceQueryLimits) -> WorkspaceListResult {
    let bounded = &raw[..raw.len().min(limits.max_output_bytes)];
    let fields: Vec<_> = bounded.split(|byte| *byte == 0).collect();
    let mut entries = Vec::new();
    let mut index = 0;
    let mut truncated = raw.len() > limits.max_output_bytes;
    while index + 2 < fields.len() && entries.len() < limits.max_results {
        let path = String::from_utf8_lossy(fields[index]);
        let relative_path = path.trim_start_matches("./");
        if relative_path.chars().count() > limits.max_line_chars {
            truncated = true;
            break;
        }
        entries.push(WorkspaceListEntry {
            relative_path: relative_path.into(),
            kind: match fields[index + 1] {
                b"f" => WorkspaceEntryKind::File,
                b"d" => WorkspaceEntryKind::Directory,
                b"l" => WorkspaceEntryKind::Symlink,
                _ => WorkspaceEntryKind::Other,
            },
            size: String::from_utf8_lossy(fields[index + 2])
                .trim()
                .parse()
                .unwrap_or(0),
        });
        index += 3;
    }
    if index + 2 < fields.len() || (!bounded.is_empty() && !bounded.ends_with(b"\0")) {
        truncated = true;
    }
    WorkspaceListResult { entries, truncated }
}

fn parse_search_output(raw: &[u8], limits: WorkspaceQueryLimits) -> WorkspaceSearchResult {
    let bounded = &raw[..raw.len().min(limits.max_output_bytes)];
    let fields: Vec<_> = bounded.split(|byte| *byte == 0).collect();
    let mut matches = Vec::new();
    let mut index = 0;
    let mut truncated = raw.len() > limits.max_output_bytes;
    while index + 2 < fields.len() && matches.len() < limits.max_results {
        let path = String::from_utf8_lossy(fields[index]);
        matches.push(WorkspaceSearchMatch {
            relative_path: path.trim_start_matches("./").into(),
            line_number: String::from_utf8_lossy(fields[index + 1])
                .parse()
                .unwrap_or(0),
            line: truncate_chars(
                &String::from_utf8_lossy(fields[index + 2]),
                limits.max_line_chars,
            ),
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

fn invalid(message: impl Into<String>) -> WorkspaceExecutorError {
    WorkspaceExecutorError::InvalidPath(message.into())
}

fn container_failure(operation: &str, output: &std::process::Output) -> WorkspaceExecutorError {
    let status = output
        .status
        .code()
        .map_or_else(|| "signal".into(), |code| code.to_string());
    WorkspaceExecutorError::Io(io::Error::other(format!(
        "container {operation} exited with {status}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
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

    struct FakeRuntime {
        directory: TempDir,
        executable: PathBuf,
        arguments: PathBuf,
        started: PathBuf,
        cleanup: PathBuf,
    }

    impl FakeRuntime {
        fn new() -> Self {
            let directory = TempDir::new().unwrap();
            let executable = directory.path().join("runtime");
            let arguments = directory.path().join("arguments");
            let started = directory.path().join("started");
            let cleanup = directory.path().join("cleanup");
            fs::write(
                &executable,
                format!(
                    r#"#!/bin/sh
printf '%s\0' "$@" > '{}'
[ "$1" = rm ] && {{ printf '%s' "$3" > '{}'; exit 0; }}
[ "$1" = run ] || exit 90
shift
mount=
while [ "$#" -gt 0 ]; do
  case $1 in
    --rm|--network=none|--interactive|--read-only) shift ;;
    --name|--memory|--cpus|--pids-limit|--tmpfs|--security-opt|--cap-drop) shift 2 ;;
    --mount) mount=$2; shift 2 ;;
    --workdir) shift 2 ;;
    *) image=$1; shift; break ;;
  esac
done
workspace=$(printf '%s' "$mount" | sed -n 's/.*source=\([^,]*\),target=.*/\1/p')
[ -n "$workspace" ] || exit 91
cd "$workspace" || exit 92
touch '{}'
exec "$@"
"#,
                    arguments.display(),
                    cleanup.display(),
                    started.display(),
                ),
            )
            .unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            Self {
                directory,
                executable,
                arguments,
                started,
                cleanup,
            }
        }

        fn executor(&self) -> ContainerExecutor {
            ContainerExecutor::new(&self.executable, "test/image:latest").unwrap()
        }

        fn target(&self, read_only: bool) -> WorkspaceTarget {
            WorkspaceTarget {
                id: "container".into(),
                workspace_path: self.directory.path().join("workspace"),
                read_only,
            }
        }

        fn argv(&self) -> Vec<String> {
            fs::read(&self.arguments)
                .unwrap()
                .split(|byte| *byte == 0)
                .filter(|field| !field.is_empty())
                .map(|field| String::from_utf8_lossy(field).into_owned())
                .collect()
        }
    }

    #[tokio::test]
    async fn file_query_and_command_contract_runs_in_disposable_container() {
        let fake = FakeRuntime::new();
        let workspace = fake.target(false).workspace_path;
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("src/lib.rs"), "alpha\nneedle\n").unwrap();
        let executor = fake.executor();
        let target = fake.target(false);

        assert_eq!(
            executor.read_file(&target, "src/lib.rs").await.unwrap(),
            b"alpha\nneedle\n"
        );
        let bounded = executor
            .read_file_bounded(&target, "src/lib.rs", 5)
            .await
            .unwrap();
        assert_eq!(bounded.bytes, b"alpha");
        assert!(bounded.truncated);
        assert_eq!(bounded.total_bytes, 13);

        executor
            .write_file(&target, "generated/value.txt", b"written")
            .await
            .unwrap();
        assert_eq!(
            fs::read(workspace.join("generated/value.txt")).unwrap(),
            b"written"
        );

        let listed = executor
            .list(
                &target,
                WorkspaceListRequest {
                    relative_path: ".".into(),
                    recursive: true,
                    limits: WorkspaceQueryLimits::default(),
                },
            )
            .await
            .unwrap();
        assert!(
            listed
                .entries
                .iter()
                .any(|entry| entry.relative_path == "src/lib.rs"),
            "{listed:?}"
        );
        assert_eq!(
            listed
                .entries
                .iter()
                .find(|entry| entry.relative_path == "src/lib.rs")
                .unwrap()
                .size,
            13
        );
        let searched = executor
            .search(
                &target,
                WorkspaceSearchRequest {
                    relative_path: ".".into(),
                    query: "needle".into(),
                    limits: WorkspaceQueryLimits::default(),
                },
            )
            .await
            .unwrap();
        assert_eq!(searched.matches[0].line, "needle");

        let progress = Arc::new(Mutex::new(String::new()));
        let captured = Arc::clone(&progress);
        let output = executor
            .run_command_streaming(
                &target,
                "printf '你好'; printf error >&2",
                Duration::from_secs(2),
                WorkspaceCommandProgressSink::new(move |_, delta| {
                    captured.lock().unwrap().push_str(&delta);
                }),
            )
            .await
            .unwrap();
        assert_eq!(output.stdout, "你好".as_bytes());
        assert_eq!(output.stderr, b"error");
        let progress = progress.lock().unwrap().clone();
        assert!(progress.contains("你好"), "{progress}");
        assert!(progress.contains("error"), "{progress}");

        let argv = fake.argv();
        assert!(argv.contains(&"--rm".into()));
        assert!(argv.contains(&"--name".into()));
        assert!(argv.contains(&"--network=none".into()));
        assert!(argv.contains(&"--memory".into()));
        assert!(argv.contains(&"2048m".into()));
        assert!(argv.contains(&"--cpus".into()));
        assert!(argv.contains(&"2.000".into()));
        assert!(argv.contains(&"--pids-limit".into()));
        assert!(argv.contains(&"512".into()));
        assert!(argv.contains(&"--read-only".into()));
        assert!(argv.contains(&"no-new-privileges".into()));
        assert!(argv.contains(&"ALL".into()));
        assert!(argv.contains(&"--interactive".into()));
        assert!(argv.contains(&"test/image:latest".into()));
    }

    #[tokio::test]
    async fn read_only_target_rejects_mutation_and_mounts_read_only_for_inspection() {
        let fake = FakeRuntime::new();
        fs::create_dir_all(fake.target(true).workspace_path).unwrap();
        let executor = fake.executor();
        let target = fake.target(true);

        assert!(matches!(
            executor.write_file(&target, "file", b"x").await,
            Err(WorkspaceExecutorError::ReadOnly(id)) if id == "container"
        ));
        let output = executor
            .run_read_only_command(&target, "printf inspected", Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(output.stdout, b"inspected");
        assert!(
            fake.argv()
                .iter()
                .any(|argument| argument.ends_with(",readonly"))
        );
    }

    #[test]
    fn runtime_and_image_cannot_be_interpreted_as_options() {
        assert!(ContainerExecutor::new("-runtime", "image").is_err());
        assert!(ContainerExecutor::new("runtime", "-image").is_err());
        assert!(ContainerExecutor::new("runtime", "image with space").is_err());
    }

    #[tokio::test]
    async fn dropping_an_operation_force_removes_its_named_container() {
        let fake = FakeRuntime::new();
        fs::create_dir_all(fake.target(false).workspace_path).unwrap();
        let executor = fake.executor();
        let target = fake.target(false);
        let operation = tokio::spawn(async move {
            executor
                .run_command(&target, "sleep 30", Duration::from_mins(1))
                .await
        });

        for _ in 0..100 {
            if fake.started.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(fake.started.exists(), "fake container never started");
        operation.abort();
        let _ = operation.await;
        for _ in 0..100 {
            if fake.cleanup.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let removed = fs::read_to_string(&fake.cleanup).unwrap();
        assert!(removed.starts_with("sylvander-"), "{removed}");
    }
}
