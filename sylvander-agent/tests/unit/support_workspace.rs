//! Test-only host workspace fixture for Agent port-contract tests.
//!
//! Production Agent code never composes this adapter. Unit tests need a real
//! filesystem/process double without depending on Runtime and creating a crate
//! cycle; Runtime separately owns and tests the production implementation.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::workspace_executor::{
    COMMAND_OUTPUT_HEAD_BYTES, MAX_COMMAND_OUTPUT_BYTES_PER_STREAM, MAX_QUERY_OUTPUT_BYTES,
    WorkspaceCommandOutput, WorkspaceCommandProgressSink, WorkspaceCommandStream,
    WorkspaceEntryKind, WorkspaceExecutor, WorkspaceExecutorError, WorkspaceListEntry,
    WorkspaceListRequest, WorkspaceListResult, WorkspaceQueryLimits, WorkspaceReadResult,
    WorkspaceSearchMatch, WorkspaceSearchRequest, WorkspaceSearchResult, WorkspaceTarget,
    validate_command_environment,
};
use async_trait::async_trait;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

/// Executor for a workspace available on the Sylvander server's filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TestWorkspaceExecutor;

#[async_trait]
impl WorkspaceExecutor for TestWorkspaceExecutor {
    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        let path = resolve_existing(target, relative_path).await?;
        Ok(tokio::fs::read(path).await?)
    }

    async fn read_file_bounded(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
        max_bytes: usize,
    ) -> Result<WorkspaceReadResult, WorkspaceExecutorError> {
        let path = resolve_existing(target, relative_path).await?;
        let file = tokio::fs::File::open(path).await?;
        let metadata_bytes = file.metadata().await?.len();
        let max_bytes_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
        let read_limit = max_bytes_u64.saturating_add(1);
        let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024).saturating_add(1));
        file.take(read_limit).read_to_end(&mut bytes).await?;
        let observed_bytes = bytes.len() as u64;
        let total_bytes = metadata_bytes.max(observed_bytes);
        let truncated = total_bytes > max_bytes_u64;
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
        run_local_command(target, command, timeout, &BTreeMap::new(), None).await
    }

    async fn run_command_with_environment(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        environment: &BTreeMap<String, String>,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        ensure_writable(target)?;
        validate_command_environment(environment)?;
        run_local_command(target, command, timeout, environment, None).await
    }

    async fn run_command_streaming(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        progress: WorkspaceCommandProgressSink,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        ensure_writable(target)?;
        run_local_command(target, command, timeout, &BTreeMap::new(), Some(progress)).await
    }

    async fn run_command_streaming_with_environment(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        environment: &BTreeMap<String, String>,
        progress: WorkspaceCommandProgressSink,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        ensure_writable(target)?;
        validate_command_environment(environment)?;
        run_local_command(target, command, timeout, environment, Some(progress)).await
    }

    async fn run_read_only_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        run_local_command(target, command, timeout, &BTreeMap::new(), None).await
    }

    async fn list(
        &self,
        target: &WorkspaceTarget,
        request: WorkspaceListRequest,
    ) -> Result<WorkspaceListResult, WorkspaceExecutorError> {
        let limits = request.limits.bounded()?;
        tokio::time::timeout(limits.timeout, list_local(target, request, limits))
            .await
            .map_err(|_| WorkspaceExecutorError::Timeout(limits.timeout))?
    }

    async fn search(
        &self,
        target: &WorkspaceTarget,
        request: WorkspaceSearchRequest,
    ) -> Result<WorkspaceSearchResult, WorkspaceExecutorError> {
        let limits = request.limits.bounded()?;
        if request.query.is_empty() {
            return Err(WorkspaceExecutorError::InvalidRequest(
                "search query must not be empty".into(),
            ));
        }
        tokio::time::timeout(limits.timeout, search_local(target, request, limits))
            .await
            .map_err(|_| WorkspaceExecutorError::Timeout(limits.timeout))?
    }
}

async fn list_local(
    target: &WorkspaceTarget,
    request: WorkspaceListRequest,
    limits: WorkspaceQueryLimits,
) -> Result<WorkspaceListResult, WorkspaceExecutorError> {
    let root = tokio::fs::canonicalize(&target.workspace_path).await?;
    let start = resolve_existing(target, &request.relative_path).await?;
    let mut pending = VecDeque::new();
    let metadata = tokio::fs::symlink_metadata(&start).await?;
    if metadata.is_dir() {
        enqueue_children(&start, &mut pending).await?;
    } else {
        pending.push_back(start);
    }

    let mut entries = Vec::new();
    let mut output_bytes = 0_usize;
    let mut truncated = false;
    while let Some(path) = pending.pop_front() {
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        let relative_path = display_relative(&root, &path)?;
        let entry_bytes = relative_path.len();
        if entries.len() == limits.max_results
            || output_bytes.saturating_add(entry_bytes) > limits.max_output_bytes
        {
            truncated = true;
            break;
        }
        output_bytes += entry_bytes;
        let file_type = metadata.file_type();
        let kind = if file_type.is_file() {
            WorkspaceEntryKind::File
        } else if file_type.is_dir() {
            WorkspaceEntryKind::Directory
        } else if file_type.is_symlink() {
            WorkspaceEntryKind::Symlink
        } else {
            WorkspaceEntryKind::Other
        };
        entries.push(WorkspaceListEntry {
            relative_path,
            kind,
            size: metadata.len(),
        });
        if request.recursive && file_type.is_dir() {
            enqueue_children(&path, &mut pending).await?;
        }
    }
    Ok(WorkspaceListResult { entries, truncated })
}

async fn search_local(
    target: &WorkspaceTarget,
    request: WorkspaceSearchRequest,
    limits: WorkspaceQueryLimits,
) -> Result<WorkspaceSearchResult, WorkspaceExecutorError> {
    let root = tokio::fs::canonicalize(&target.workspace_path).await?;
    let start = resolve_existing(target, &request.relative_path).await?;
    let mut pending = VecDeque::from([start]);
    let mut matches = Vec::new();
    let mut output_bytes = 0_usize;
    let mut truncated = false;

    'paths: while let Some(path) = pending.pop_front() {
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        if metadata.is_dir() {
            enqueue_children(&path, &mut pending).await?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let relative_path = display_relative(&root, &path)?;
        let file = tokio::fs::File::open(path).await?;
        let mut reader = BufReader::new(file);
        let mut line_number = 0_u64;
        while let Some(bytes) = read_bounded_line(&mut reader).await? {
            line_number += 1;
            let text = String::from_utf8_lossy(&bytes);
            if !text.contains(&request.query) {
                continue;
            }
            let line = truncate_chars(text.trim_end_matches(['\r', '\n']), limits.max_line_chars);
            let match_bytes = relative_path.len() + line.len();
            if matches.len() == limits.max_results
                || output_bytes.saturating_add(match_bytes) > limits.max_output_bytes
            {
                truncated = true;
                break 'paths;
            }
            output_bytes += match_bytes;
            matches.push(WorkspaceSearchMatch {
                relative_path: relative_path.clone(),
                line_number,
                line,
            });
        }
    }
    Ok(WorkspaceSearchResult { matches, truncated })
}

async fn enqueue_children(
    directory: &Path,
    pending: &mut VecDeque<PathBuf>,
) -> Result<(), WorkspaceExecutorError> {
    let mut reader = tokio::fs::read_dir(directory).await?;
    let mut children = Vec::new();
    while let Some(entry) = reader.next_entry().await? {
        if entry.file_name() == ".git" {
            continue;
        }
        children.push(entry.path());
    }
    children.sort();
    pending.extend(children);
    Ok(())
}

fn display_relative(root: &Path, path: &Path) -> Result<String, WorkspaceExecutorError> {
    path.strip_prefix(root)
        .map(|relative| {
            relative
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        })
        .map_err(|_| WorkspaceExecutorError::InvalidPath(path.display().to_string()))
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut characters = value.chars();
    let mut result: String = characters.by_ref().take(max_chars).collect();
    if characters.next().is_some() {
        result.push('…');
    }
    result
}

async fn read_bounded_line(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Option<Vec<u8>>, WorkspaceExecutorError> {
    let mut line = Vec::new();
    let mut saw_bytes = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(saw_bytes.then_some(line));
        }
        saw_bytes = true;
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        let retained = consumed.min(MAX_QUERY_OUTPUT_BYTES.saturating_sub(line.len()));
        line.extend_from_slice(&available[..retained]);
        let complete = available[..consumed].ends_with(b"\n");
        reader.consume(consumed);
        if complete {
            return Ok(Some(line));
        }
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
    process.as_std_mut().process_group(0);
    process
}

#[cfg(unix)]
struct ProcessGroupGuard {
    process_group: i32,
    armed: bool,
}

#[cfg(unix)]
impl ProcessGroupGuard {
    fn new(process_group: i32) -> Self {
        Self {
            process_group,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if self.armed {
            // A negative pid addresses the whole process group. Use the
            // platform utility because this workspace forbids unsafe FFI.
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", self.process_group)])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

async fn run_local_command(
    target: &WorkspaceTarget,
    command: &str,
    timeout: Duration,
    environment: &BTreeMap<String, String>,
    progress: Option<WorkspaceCommandProgressSink>,
) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
    let root = tokio::fs::canonicalize(&target.workspace_path).await?;
    if !root.is_dir() {
        return Err(WorkspaceExecutorError::InvalidPath(format!(
            "workspace is not a directory: {}",
            root.display()
        )));
    }
    let mut process = shell_command(command);
    process
        .current_dir(root)
        .envs(environment)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = process.spawn()?;
    #[cfg(unix)]
    let mut process_group = ProcessGroupGuard::new(
        child
            .id()
            .ok_or_else(|| std::io::Error::other("command process has no pid"))?
            .try_into()
            .map_err(|_| std::io::Error::other("command pid exceeds i32"))?,
    );
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("command stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("command stderr was not piped"))?;
    let execution = async move {
        let result = tokio::try_join!(
            capture_command_output(
                stdout,
                progress
                    .clone()
                    .map(|sink| (WorkspaceCommandStream::Stdout, sink)),
            ),
            capture_command_output(
                stderr,
                progress.map(|sink| (WorkspaceCommandStream::Stderr, sink)),
            ),
            child.wait(),
        );
        #[cfg(unix)]
        if result.is_ok() {
            process_group.disarm();
        }
        result
    };
    let (stdout, stderr, status) = Box::pin(tokio::time::timeout(timeout, execution))
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

struct CapturedCommandOutput {
    bytes: Vec<u8>,
    total_bytes: u64,
    truncated: bool,
}

async fn capture_command_output(
    mut reader: impl AsyncRead + Unpin,
    progress: Option<(WorkspaceCommandStream, WorkspaceCommandProgressSink)>,
) -> std::io::Result<CapturedCommandOutput> {
    let tail_capacity =
        MAX_COMMAND_OUTPUT_BYTES_PER_STREAM.saturating_sub(COMMAND_OUTPUT_HEAD_BYTES);
    let mut head = Vec::with_capacity(COMMAND_OUTPUT_HEAD_BYTES);
    let mut tail = VecDeque::<u8>::with_capacity(tail_capacity);
    let mut total_bytes = 0_u64;
    let mut buffer = [0_u8; 8 * 1024];
    let mut utf8_pending = Vec::new();

    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        if let Some((stream, sink)) = &progress {
            emit_utf8_progress(*stream, sink, &mut utf8_pending, &buffer[..read], false);
        }
        total_bytes = total_bytes.saturating_add(read as u64);
        let mut chunk = &buffer[..read];
        if head.len() < COMMAND_OUTPUT_HEAD_BYTES {
            let keep = chunk
                .len()
                .min(COMMAND_OUTPUT_HEAD_BYTES.saturating_sub(head.len()));
            head.extend_from_slice(&chunk[..keep]);
            chunk = &chunk[keep..];
        }
        if tail_capacity > 0 && !chunk.is_empty() {
            tail.extend(chunk);
            if tail.len() > tail_capacity {
                tail.drain(..tail.len() - tail_capacity);
            }
        }
    }
    if let Some((stream, sink)) = &progress {
        emit_utf8_progress(*stream, sink, &mut utf8_pending, &[], true);
    }

    let mut bytes = head;
    bytes.extend(tail);
    Ok(CapturedCommandOutput {
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
        if let Err(error) = std::str::from_utf8(&pending[offset..]) {
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
        } else {
            sink.emit(
                stream,
                String::from_utf8_lossy(&pending[offset..]).into_owned(),
            );
            offset = pending.len();
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

#[cfg(windows)]
fn shell_command(command: &str) -> tokio::process::Command {
    let mut process = tokio::process::Command::new("cmd");
    process.args(["/C", command]);
    process
}
