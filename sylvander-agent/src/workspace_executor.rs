//! Location-neutral workspace operations used by coding tools.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};

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
pub struct WorkspaceCommandOutput {
    pub success: bool,
    pub status_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
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
        self.run_read_only_command(target, command, timeout).await
    }

    async fn run_read_only_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
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

    async fn run_read_only_command(
        &self,
        _target: &WorkspaceTarget,
        _command: &str,
        _timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        Err(self.error())
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
    async fn local_list_and_search_are_bounded_unicode_safe_and_read_only() {
        let workspace = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(workspace.path().join("子目录"))
            .await
            .unwrap();
        tokio::fs::write(
            workspace.path().join("子目录/说明.txt"),
            "第一行\n匹配：螃蟹伙伴很可靠\n匹配：第二处\n",
        )
        .await
        .unwrap();
        let target = WorkspaceTarget::local(workspace.path(), true);
        let executor = LocalExecutor;

        let list = executor
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
            list.entries
                .iter()
                .any(|entry| entry.relative_path == "子目录/说明.txt")
        );

        let search = executor
            .search(
                &target,
                WorkspaceSearchRequest {
                    relative_path: ".".into(),
                    query: "匹配".into(),
                    limits: WorkspaceQueryLimits {
                        max_results: 1,
                        max_line_chars: 6,
                        ..WorkspaceQueryLimits::default()
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(search.matches.len(), 1);
        assert_eq!(search.matches[0].relative_path, "子目录/说明.txt");
        assert_eq!(search.matches[0].line, "匹配：螃蟹伙…");
        assert!(search.truncated);

        let output = executor
            .run_read_only_command(&target, "printf readonly-ok", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(output.stdout, b"readonly-ok");
    }

    #[tokio::test]
    async fn local_query_limits_are_clamped_and_zero_is_rejected() {
        let bounded = WorkspaceQueryLimits {
            max_results: usize::MAX,
            max_line_chars: usize::MAX,
            max_output_bytes: usize::MAX,
            timeout: Duration::MAX,
        }
        .bounded()
        .unwrap();
        assert_eq!(bounded.max_results, MAX_QUERY_RESULTS);
        assert_eq!(bounded.max_line_chars, MAX_QUERY_LINE_CHARS);
        assert_eq!(bounded.max_output_bytes, MAX_QUERY_OUTPUT_BYTES);
        assert_eq!(bounded.timeout, MAX_QUERY_TIMEOUT);
        assert!(
            WorkspaceQueryLimits {
                max_results: 0,
                ..WorkspaceQueryLimits::default()
            }
            .bounded()
            .is_err()
        );
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
