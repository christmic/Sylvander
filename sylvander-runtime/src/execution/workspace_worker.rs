//! Server-side proxy for an outbound-connected macOS workspace worker.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use sylvander_agent::workspace_executor::{
    ProcessIsolation, WorkspaceCommandOutput, WorkspaceCommandProgressSink, WorkspaceCommandStream,
    WorkspaceEntryKind, WorkspaceExecutor, WorkspaceExecutorError, WorkspaceFileRevision,
    WorkspaceListEntry, WorkspaceListRequest, WorkspaceListResult, WorkspaceReadResult,
    WorkspaceSearchMatch, WorkspaceSearchRequest, WorkspaceSearchResult, WorkspaceTarget,
    validate_command_environment,
};
use sylvander_api::{
    WORKSPACE_WORKER_PROTOCOL_VERSION, WorkspaceWorkerEntryKind, WorkspaceWorkerErrorCode,
    WorkspaceWorkerEvent, WorkspaceWorkerHello, WorkspaceWorkerListEntry, WorkspaceWorkerOperation,
    WorkspaceWorkerQueryLimits, WorkspaceWorkerRequest, WorkspaceWorkerResult,
    WorkspaceWorkerSearchMatch, WorkspaceWorkerServerMessage, WorkspaceWorkerStream,
};
use tokio::sync::{Mutex, mpsc, oneshot};

type PendingResult = Result<WorkspaceWorkerResult, WorkspaceWorkerErrorCode>;

struct WorkerConnection {
    generation: String,
    requests: mpsc::Sender<WorkspaceWorkerServerMessage>,
}

struct PendingRequest {
    target_id: String,
    generation: String,
    result: oneshot::Sender<PendingResult>,
    progress: Option<WorkspaceCommandProgressSink>,
}

#[derive(Default)]
struct HubState {
    connections: HashMap<String, WorkerConnection>,
    pending: HashMap<String, PendingRequest>,
}

/// Process-wide rendezvous used by authenticated channels and Runtime proxies.
#[derive(Clone, Default)]
pub(crate) struct WorkspaceWorkerHub {
    state: Arc<Mutex<HubState>>,
}

impl WorkspaceWorkerHub {
    pub(crate) fn global() -> Self {
        static HUB: OnceLock<WorkspaceWorkerHub> = OnceLock::new();
        HUB.get_or_init(Self::default).clone()
    }

    pub(crate) async fn connect(
        &self,
        hello: &WorkspaceWorkerHello,
        requests: mpsc::Sender<WorkspaceWorkerServerMessage>,
    ) -> Result<String, ()> {
        if hello.protocol_version != WORKSPACE_WORKER_PROTOCOL_VERSION
            || hello.target_id.trim().is_empty()
            || hello.workspace_root.trim().is_empty()
            || hello.allow_local_fallback
        {
            return Err(());
        }
        let generation = uuid::Uuid::new_v4().to_string();
        let mut state = self.state.lock().await;
        if state.connections.contains_key(&hello.target_id) {
            return Err(());
        }
        state.connections.insert(
            hello.target_id.clone(),
            WorkerConnection {
                generation: generation.clone(),
                requests,
            },
        );
        Ok(generation)
    }

    pub(crate) async fn event(&self, generation: &str, event: WorkspaceWorkerEvent) {
        let request_id = match &event {
            WorkspaceWorkerEvent::Progress { request_id, .. }
            | WorkspaceWorkerEvent::Complete { request_id, .. }
            | WorkspaceWorkerEvent::Failed { request_id, .. } => request_id,
        }
        .clone();
        let mut state = self.state.lock().await;
        let Some(pending) = state.pending.get(&request_id) else {
            return;
        };
        if pending.generation != generation {
            return;
        }
        match event {
            WorkspaceWorkerEvent::Progress { stream, delta, .. } => {
                if let Some(progress) = &pending.progress {
                    progress.emit(map_stream(stream), delta);
                }
            }
            WorkspaceWorkerEvent::Complete { result, .. } => {
                if let Some(pending) = state.pending.remove(&request_id) {
                    let _ = pending.result.send(Ok(result));
                }
            }
            WorkspaceWorkerEvent::Failed { code, .. } => {
                if let Some(pending) = state.pending.remove(&request_id) {
                    let _ = pending.result.send(Err(code));
                }
            }
        }
    }

    pub(crate) async fn disconnect(&self, target_id: &str, generation: &str) {
        let mut state = self.state.lock().await;
        if state
            .connections
            .get(target_id)
            .is_some_and(|item| item.generation == generation)
        {
            state.connections.remove(target_id);
            let ids = state
                .pending
                .iter()
                .filter(|(_, item)| item.target_id == target_id && item.generation == generation)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            for id in ids {
                if let Some(item) = state.pending.remove(&id) {
                    let _ = item
                        .result
                        .send(Err(WorkspaceWorkerErrorCode::Disconnected));
                }
            }
        }
    }

    async fn cancel(&self, target_id: &str, generation: &str, request_id: &str) {
        let requests = {
            let mut state = self.state.lock().await;
            let Some(pending) = state.pending.get(request_id) else {
                return;
            };
            if pending.target_id != target_id || pending.generation != generation {
                return;
            }
            state.pending.remove(request_id);
            state
                .connections
                .get(target_id)
                .filter(|item| item.generation == generation)
                .map(|item| item.requests.clone())
        };
        if let Some(requests) = requests {
            let _ = requests
                .send(WorkspaceWorkerServerMessage::Cancel {
                    request_id: request_id.to_owned(),
                })
                .await;
        }
    }

    async fn request(
        &self,
        target_id: &str,
        workspace: String,
        operation: WorkspaceWorkerOperation,
        progress: Option<WorkspaceCommandProgressSink>,
    ) -> Result<WorkspaceWorkerResult, WorkspaceExecutorError> {
        validate_workspace(&workspace)?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        let (requests, generation) = {
            let mut state = self.state.lock().await;
            let connection = state
                .connections
                .get(target_id)
                .ok_or_else(|| WorkspaceExecutorError::Unavailable(target_id.to_owned()))?;
            let requests = connection.requests.clone();
            let generation = connection.generation.clone();
            state.pending.insert(
                request_id.clone(),
                PendingRequest {
                    target_id: target_id.to_owned(),
                    generation: generation.clone(),
                    result: sender,
                    progress,
                },
            );
            (requests, generation)
        };
        let mut cancellation = RequestCancellation {
            hub: self.clone(),
            target_id: target_id.to_owned(),
            generation: generation.clone(),
            request_id: request_id.clone(),
            armed: true,
        };
        if requests
            .send(WorkspaceWorkerServerMessage::Request {
                request: WorkspaceWorkerRequest {
                    request_id: request_id.clone(),
                    workspace,
                    operation,
                },
            })
            .await
            .is_err()
        {
            self.disconnect(target_id, &generation).await;
        }
        let result = receiver
            .await
            .map_err(|_| WorkspaceExecutorError::Unavailable(target_id.to_owned()))?
            .map_err(|code| map_error(target_id, code));
        cancellation.armed = false;
        result
    }
}

struct RequestCancellation {
    hub: WorkspaceWorkerHub,
    target_id: String,
    generation: String,
    request_id: String,
    armed: bool,
}

impl Drop for RequestCancellation {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let hub = self.hub.clone();
        let target_id = self.target_id.clone();
        let generation = self.generation.clone();
        let request_id = self.request_id.clone();
        tokio::spawn(async move {
            hub.cancel(&target_id, &generation, &request_id).await;
        });
    }
}

#[derive(Clone)]
pub(crate) struct WorkspaceWorkerExecutor {
    target_id: String,
    hub: WorkspaceWorkerHub,
}

impl std::fmt::Debug for WorkspaceWorkerExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceWorkerExecutor")
            .field("target_id", &self.target_id)
            .finish_non_exhaustive()
    }
}

impl WorkspaceWorkerExecutor {
    pub(crate) fn new(target_id: String) -> Self {
        Self {
            target_id,
            hub: WorkspaceWorkerHub::global(),
        }
    }

    fn workspace(target: &WorkspaceTarget) -> Result<String, WorkspaceExecutorError> {
        let value = target.workspace_path.to_string_lossy().into_owned();
        validate_workspace(&value)?;
        Ok(value)
    }
}

#[async_trait]
impl WorkspaceExecutor for WorkspaceWorkerExecutor {
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
        path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        let result = self.read_file_bounded(target, path, usize::MAX).await?;
        Ok(result.bytes)
    }

    async fn read_file_bounded(
        &self,
        target: &WorkspaceTarget,
        path: &str,
        max_bytes: usize,
    ) -> Result<WorkspaceReadResult, WorkspaceExecutorError> {
        match self
            .hub
            .request(
                &self.target_id,
                Self::workspace(target)?,
                WorkspaceWorkerOperation::Read {
                    path: path.into(),
                    max_bytes: Some(max_bytes),
                },
                None,
            )
            .await?
        {
            WorkspaceWorkerResult::Read {
                bytes,
                total_bytes,
                truncated,
            } => Ok(WorkspaceReadResult {
                bytes,
                total_bytes,
                truncated,
            }),
            _ => Err(WorkspaceExecutorError::InvalidRequest(
                "workspace worker returned a mismatched result".into(),
            )),
        }
    }

    async fn write_file(
        &self,
        target: &WorkspaceTarget,
        path: &str,
        bytes: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        if target.read_only {
            return Err(WorkspaceExecutorError::ReadOnly(target.id.clone()));
        }
        match self
            .hub
            .request(
                &self.target_id,
                Self::workspace(target)?,
                WorkspaceWorkerOperation::Write {
                    path: path.into(),
                    bytes: bytes.into(),
                },
                None,
            )
            .await?
        {
            WorkspaceWorkerResult::Written => Ok(()),
            _ => Err(WorkspaceExecutorError::InvalidRequest(
                "workspace worker returned a mismatched result".into(),
            )),
        }
    }

    async fn write_file_if_revision(
        &self,
        target: &WorkspaceTarget,
        path: &str,
        expected: &WorkspaceFileRevision,
        bytes: &[u8],
        max_bytes: usize,
    ) -> Result<(), WorkspaceExecutorError> {
        if target.read_only {
            return Err(WorkspaceExecutorError::ReadOnly(target.id.clone()));
        }
        match self
            .hub
            .request(
                &self.target_id,
                Self::workspace(target)?,
                WorkspaceWorkerOperation::WriteIfRevision {
                    path: path.into(),
                    expected_sha256: expected.as_bytes().to_vec(),
                    bytes: bytes.into(),
                    max_bytes,
                },
                None,
            )
            .await?
        {
            WorkspaceWorkerResult::Written => Ok(()),
            _ => Err(WorkspaceExecutorError::InvalidRequest(
                "workspace worker returned a mismatched result".into(),
            )),
        }
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
        self.command(target, command, timeout, environment, None, false)
            .await
    }

    async fn run_command_streaming(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        progress: WorkspaceCommandProgressSink,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        self.command(
            target,
            command,
            timeout,
            &BTreeMap::new(),
            Some(progress),
            false,
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
        self.command(target, command, timeout, environment, Some(progress), false)
            .await
    }

    async fn run_read_only_command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        self.command(target, command, timeout, &BTreeMap::new(), None, true)
            .await
    }

    async fn list(
        &self,
        target: &WorkspaceTarget,
        request: WorkspaceListRequest,
    ) -> Result<WorkspaceListResult, WorkspaceExecutorError> {
        let limits = map_limits(request.limits);
        match self
            .hub
            .request(
                &self.target_id,
                Self::workspace(target)?,
                WorkspaceWorkerOperation::List {
                    path: request.relative_path,
                    recursive: request.recursive,
                    limits,
                },
                None,
            )
            .await?
        {
            WorkspaceWorkerResult::List { entries, truncated } => Ok(WorkspaceListResult {
                entries: entries.into_iter().map(map_entry).collect(),
                truncated,
            }),
            _ => Err(WorkspaceExecutorError::InvalidRequest(
                "workspace worker returned a mismatched result".into(),
            )),
        }
    }

    async fn search(
        &self,
        target: &WorkspaceTarget,
        request: WorkspaceSearchRequest,
    ) -> Result<WorkspaceSearchResult, WorkspaceExecutorError> {
        let limits = map_limits(request.limits);
        match self
            .hub
            .request(
                &self.target_id,
                Self::workspace(target)?,
                WorkspaceWorkerOperation::Search {
                    path: request.relative_path,
                    query: request.query,
                    limits,
                },
                None,
            )
            .await?
        {
            WorkspaceWorkerResult::Search { matches, truncated } => Ok(WorkspaceSearchResult {
                matches: matches.into_iter().map(map_match).collect(),
                truncated,
            }),
            _ => Err(WorkspaceExecutorError::InvalidRequest(
                "workspace worker returned a mismatched result".into(),
            )),
        }
    }
}

impl WorkspaceWorkerExecutor {
    async fn command(
        &self,
        target: &WorkspaceTarget,
        command: &str,
        timeout: Duration,
        environment: &BTreeMap<String, String>,
        progress: Option<WorkspaceCommandProgressSink>,
        read_only: bool,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        if target.read_only && !read_only {
            return Err(WorkspaceExecutorError::ReadOnly(target.id.clone()));
        }
        let timeout_millis = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        match self
            .hub
            .request(
                &self.target_id,
                Self::workspace(target)?,
                WorkspaceWorkerOperation::Command {
                    command: command.into(),
                    timeout_millis,
                    environment: environment.clone(),
                    read_only,
                },
                progress,
            )
            .await?
        {
            WorkspaceWorkerResult::Command {
                success,
                status_code,
                stdout,
                stderr,
                stdout_truncated,
                stderr_truncated,
                stdout_total_bytes,
                stderr_total_bytes,
            } => Ok(WorkspaceCommandOutput {
                success,
                status_code,
                stdout,
                stderr,
                stdout_truncated,
                stderr_truncated,
                stdout_total_bytes,
                stderr_total_bytes,
            }),
            _ => Err(WorkspaceExecutorError::InvalidRequest(
                "workspace worker returned a mismatched result".into(),
            )),
        }
    }
}

fn validate_workspace(value: &str) -> Result<(), WorkspaceExecutorError> {
    let path = std::path::Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(WorkspaceExecutorError::InvalidPath(
            "client workspace must be a normalized relative path".into(),
        ));
    }
    Ok(())
}

fn map_stream(value: WorkspaceWorkerStream) -> WorkspaceCommandStream {
    match value {
        WorkspaceWorkerStream::Stdout => WorkspaceCommandStream::Stdout,
        WorkspaceWorkerStream::Stderr => WorkspaceCommandStream::Stderr,
    }
}
fn map_limits(
    value: sylvander_agent::workspace_executor::WorkspaceQueryLimits,
) -> WorkspaceWorkerQueryLimits {
    WorkspaceWorkerQueryLimits {
        max_results: value.max_results,
        max_line_chars: value.max_line_chars,
        max_output_bytes: value.max_output_bytes,
        timeout_millis: u64::try_from(value.timeout.as_millis()).unwrap_or(u64::MAX),
    }
}
fn map_entry(value: WorkspaceWorkerListEntry) -> WorkspaceListEntry {
    WorkspaceListEntry {
        relative_path: value.path,
        kind: match value.kind {
            WorkspaceWorkerEntryKind::File => WorkspaceEntryKind::File,
            WorkspaceWorkerEntryKind::Directory => WorkspaceEntryKind::Directory,
            WorkspaceWorkerEntryKind::Symlink => WorkspaceEntryKind::Symlink,
            WorkspaceWorkerEntryKind::Other => WorkspaceEntryKind::Other,
        },
        size: value.size,
    }
}
fn map_match(value: WorkspaceWorkerSearchMatch) -> WorkspaceSearchMatch {
    WorkspaceSearchMatch {
        relative_path: value.path,
        line_number: value.line_number,
        line: value.line,
    }
}
fn map_error(target: &str, code: WorkspaceWorkerErrorCode) -> WorkspaceExecutorError {
    match code {
        WorkspaceWorkerErrorCode::ReadOnly => WorkspaceExecutorError::ReadOnly(target.into()),
        WorkspaceWorkerErrorCode::PathBoundary => WorkspaceExecutorError::PolicyViolation(
            sylvander_agent::workspace_executor::WorkspacePolicyViolation::FilesystemBoundary,
        ),
        WorkspaceWorkerErrorCode::Conflict => {
            WorkspaceExecutorError::WriteConflict("remote file changed".into())
        }
        WorkspaceWorkerErrorCode::Timeout => WorkspaceExecutorError::Timeout(Duration::ZERO),
        WorkspaceWorkerErrorCode::Disconnected => {
            WorkspaceExecutorError::Unavailable(target.into())
        }
        WorkspaceWorkerErrorCode::InvalidRequest | WorkspaceWorkerErrorCode::Execution => {
            WorkspaceExecutorError::InvalidRequest("workspace worker rejected the operation".into())
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/execution_workspace_worker.rs"]
mod tests;
