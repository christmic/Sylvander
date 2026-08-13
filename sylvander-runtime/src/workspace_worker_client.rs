//! Outbound client that exposes one granted macOS workspace through Seatbelt.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use sylvander_agent::workspace_executor::{
    WorkspaceCommandProgressSink, WorkspaceCommandStream, WorkspaceEntryKind, WorkspaceExecutor,
    WorkspaceFileRevision, WorkspaceListRequest, WorkspaceQueryLimits, WorkspaceSearchRequest,
    WorkspaceTarget,
};
use sylvander_api::{
    WORKSPACE_WORKER_PROTOCOL_VERSION, WorkspaceWorkerClientMessage, WorkspaceWorkerEntryKind,
    WorkspaceWorkerErrorCode, WorkspaceWorkerEvent, WorkspaceWorkerHello, WorkspaceWorkerListEntry,
    WorkspaceWorkerOperation, WorkspaceWorkerQueryLimits, WorkspaceWorkerRequest,
    WorkspaceWorkerResult, WorkspaceWorkerSearchMatch, WorkspaceWorkerServerMessage,
    WorkspaceWorkerStream,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

#[derive(Debug, Clone)]
pub struct WorkspaceWorkerClientConfig {
    pub endpoint: String,
    pub bearer_token: String,
    pub target_id: String,
    pub workspace_root: PathBuf,
    pub allow_local_fallback: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceWorkerClientError {
    #[error("workspace worker configuration is invalid")]
    InvalidConfiguration,
    #[error("workspace worker transport failed")]
    Transport,
    #[error("workspace worker protocol failed")]
    Protocol,
    #[error("macOS Seatbelt is unavailable and local fallback is disabled")]
    SandboxUnavailable,
}

pub async fn run_workspace_worker(
    config: WorkspaceWorkerClientConfig,
) -> Result<(), WorkspaceWorkerClientError> {
    validate_config(&config)?;
    let root = tokio::fs::canonicalize(&config.workspace_root)
        .await
        .map_err(|_| WorkspaceWorkerClientError::InvalidConfiguration)?;
    if !root.is_dir() {
        return Err(WorkspaceWorkerClientError::InvalidConfiguration);
    }
    let executor = worker_executor(config.allow_local_fallback)?;
    let mut request = config
        .endpoint
        .clone()
        .into_client_request()
        .map_err(|_| WorkspaceWorkerClientError::InvalidConfiguration)?;
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", config.bearer_token))
            .map_err(|_| WorkspaceWorkerClientError::InvalidConfiguration)?,
    );
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|_| WorkspaceWorkerClientError::Transport)?;
    let (mut writer, mut reader) = socket.split();
    let hello = WorkspaceWorkerClientMessage::Hello {
        worker: WorkspaceWorkerHello {
            protocol_version: WORKSPACE_WORKER_PROTOCOL_VERSION,
            target_id: config.target_id,
            workspace_root: root.to_string_lossy().into_owned(),
            allow_local_fallback: config.allow_local_fallback,
        },
    };
    send_json(&mut writer, &hello).await?;
    let Some(Ok(Message::Text(welcome))) = reader.next().await else {
        return Err(WorkspaceWorkerClientError::Protocol);
    };
    if !matches!(
        serde_json::from_str(&welcome),
        Ok(WorkspaceWorkerServerMessage::Welcome {
            protocol_version: WORKSPACE_WORKER_PROTOCOL_VERSION
        })
    ) {
        return Err(WorkspaceWorkerClientError::Protocol);
    }

    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let mut active: HashMap<String, JoinHandle<()>> = HashMap::new();
    loop {
        tokio::select! {
            event = events_rx.recv() => {
                let Some(event) = event else { break };
                send_json(&mut writer, &WorkspaceWorkerClientMessage::Event { event }).await?;
            }
            incoming = reader.next() => {
                let Some(Ok(Message::Text(incoming))) = incoming else { break };
                match serde_json::from_str::<WorkspaceWorkerServerMessage>(&incoming)
                    .map_err(|_| WorkspaceWorkerClientError::Protocol)? {
                    WorkspaceWorkerServerMessage::Request { request } => {
                        let request_id = request.request_id.clone();
                        let root = root.clone();
                        let executor = executor.clone();
                        let events = events_tx.clone();
                        let task = tokio::spawn(async move {
                            let event = execute_request(executor, &root, request, events.clone()).await;
                            let _ = events.send(event);
                        });
                        if let Some(old) = active.insert(request_id, task) { old.abort(); }
                    }
                    WorkspaceWorkerServerMessage::Cancel { request_id } => {
                        if let Some(task) = active.remove(&request_id) { task.abort(); }
                    }
                    WorkspaceWorkerServerMessage::Welcome { .. } => return Err(WorkspaceWorkerClientError::Protocol),
                }
                active.retain(|_, task| !task.is_finished());
            }
        }
    }
    for (_, task) in active {
        task.abort();
    }
    Err(WorkspaceWorkerClientError::Transport)
}

async fn execute_request(
    executor: Arc<dyn WorkspaceExecutor>,
    root: &Path,
    request: WorkspaceWorkerRequest,
    events: mpsc::UnboundedSender<WorkspaceWorkerEvent>,
) -> WorkspaceWorkerEvent {
    let request_id = request.request_id;
    let workspace = match resolve_workspace(root, &request.workspace).await {
        Ok(value) => value,
        Err(code) => return WorkspaceWorkerEvent::Failed { request_id, code },
    };
    let target = WorkspaceTarget::local(workspace, false);
    let result = match request.operation {
        WorkspaceWorkerOperation::Read { path, max_bytes } => executor
            .read_file_bounded(&target, &path, max_bytes.unwrap_or(usize::MAX))
            .await
            .map(|read| WorkspaceWorkerResult::Read {
                bytes: read.bytes,
                total_bytes: read.total_bytes,
                truncated: read.truncated,
            }),
        WorkspaceWorkerOperation::Write { path, bytes } => executor
            .write_file(&target, &path, &bytes)
            .await
            .map(|()| WorkspaceWorkerResult::Written),
        WorkspaceWorkerOperation::WriteIfRevision {
            path,
            expected_sha256,
            bytes,
            max_bytes,
        } => {
            let expected: Result<[u8; 32], _> = expected_sha256.try_into();
            match expected {
                Ok(expected) => executor
                    .write_file_if_revision(
                        &target,
                        &path,
                        &WorkspaceFileRevision::from_bytes(expected),
                        &bytes,
                        max_bytes,
                    )
                    .await
                    .map(|()| WorkspaceWorkerResult::Written),
                Err(_) => {
                    return WorkspaceWorkerEvent::Failed {
                        request_id,
                        code: WorkspaceWorkerErrorCode::InvalidRequest,
                    };
                }
            }
        }
        WorkspaceWorkerOperation::List {
            path,
            recursive,
            limits,
        } => executor
            .list(
                &target,
                WorkspaceListRequest {
                    relative_path: path,
                    recursive,
                    limits: query_limits(limits),
                },
            )
            .await
            .map(|list| WorkspaceWorkerResult::List {
                entries: list
                    .entries
                    .into_iter()
                    .map(|entry| WorkspaceWorkerListEntry {
                        path: entry.relative_path,
                        kind: entry_kind(entry.kind),
                        size: entry.size,
                    })
                    .collect(),
                truncated: list.truncated,
            }),
        WorkspaceWorkerOperation::Search {
            path,
            query,
            limits,
        } => executor
            .search(
                &target,
                WorkspaceSearchRequest {
                    relative_path: path,
                    query,
                    limits: query_limits(limits),
                },
            )
            .await
            .map(|search| WorkspaceWorkerResult::Search {
                matches: search
                    .matches
                    .into_iter()
                    .map(|item| WorkspaceWorkerSearchMatch {
                        path: item.relative_path,
                        line_number: item.line_number,
                        line: item.line,
                    })
                    .collect(),
                truncated: search.truncated,
            }),
        WorkspaceWorkerOperation::Command {
            command,
            timeout_millis,
            environment,
            read_only,
        } => {
            let progress_id = request_id.clone();
            let progress_events = events;
            let progress = WorkspaceCommandProgressSink::new(move |stream, delta| {
                let _ = progress_events.send(WorkspaceWorkerEvent::Progress {
                    request_id: progress_id.clone(),
                    stream: match stream {
                        WorkspaceCommandStream::Stdout => WorkspaceWorkerStream::Stdout,
                        WorkspaceCommandStream::Stderr => WorkspaceWorkerStream::Stderr,
                    },
                    delta,
                });
            });
            let timeout = Duration::from_millis(timeout_millis.max(1));
            let output = if read_only {
                let read_only_target = WorkspaceTarget::local(&target.workspace_path, true);
                executor
                    .run_read_only_command(&read_only_target, &command, timeout)
                    .await
            } else {
                executor
                    .run_command_streaming_with_environment(
                        &target,
                        &command,
                        timeout,
                        &environment,
                        progress,
                    )
                    .await
            };
            output.map(|value| WorkspaceWorkerResult::Command {
                success: value.success,
                status_code: value.status_code,
                stdout: value.stdout,
                stderr: value.stderr,
                stdout_truncated: value.stdout_truncated,
                stderr_truncated: value.stderr_truncated,
                stdout_total_bytes: value.stdout_total_bytes,
                stderr_total_bytes: value.stderr_total_bytes,
            })
        }
    };
    match result {
        Ok(result) => WorkspaceWorkerEvent::Complete { request_id, result },
        Err(error) => WorkspaceWorkerEvent::Failed {
            request_id,
            code: error_code(&error),
        },
    }
}

fn validate_config(config: &WorkspaceWorkerClientConfig) -> Result<(), WorkspaceWorkerClientError> {
    if config.target_id.trim().is_empty()
        || config.bearer_token.is_empty()
        || !config.endpoint.starts_with("wss://")
    {
        return Err(WorkspaceWorkerClientError::InvalidConfiguration);
    }
    Ok(())
}

async fn resolve_workspace(
    root: &Path,
    relative: &str,
) -> Result<PathBuf, WorkspaceWorkerErrorCode> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|item| matches!(item, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(WorkspaceWorkerErrorCode::PathBoundary);
    }
    let resolved = tokio::fs::canonicalize(root.join(relative))
        .await
        .map_err(|_| WorkspaceWorkerErrorCode::PathBoundary)?;
    resolved
        .starts_with(root)
        .then_some(resolved)
        .ok_or(WorkspaceWorkerErrorCode::PathBoundary)
}

#[cfg(target_os = "macos")]
fn worker_executor(
    allow_local_fallback: bool,
) -> Result<Arc<dyn WorkspaceExecutor>, WorkspaceWorkerClientError> {
    if crate::execution::MacosSeatbeltExecutor::available() {
        return Ok(Arc::new(crate::execution::MacosSeatbeltExecutor));
    }
    if allow_local_fallback {
        return Ok(Arc::new(crate::execution::LocalExecutor));
    }
    Err(WorkspaceWorkerClientError::SandboxUnavailable)
}

#[cfg(not(target_os = "macos"))]
fn worker_executor(
    _allow_local_fallback: bool,
) -> Result<Arc<dyn WorkspaceExecutor>, WorkspaceWorkerClientError> {
    Err(WorkspaceWorkerClientError::SandboxUnavailable)
}

async fn send_json<S, T>(writer: &mut S, value: &T) -> Result<(), WorkspaceWorkerClientError>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::fmt::Debug,
    T: serde::Serialize,
{
    let encoded = serde_json::to_string(value).map_err(|_| WorkspaceWorkerClientError::Protocol)?;
    writer
        .send(Message::Text(encoded.into()))
        .await
        .map_err(|_| WorkspaceWorkerClientError::Transport)
}

fn query_limits(value: WorkspaceWorkerQueryLimits) -> WorkspaceQueryLimits {
    WorkspaceQueryLimits {
        max_results: value.max_results,
        max_line_chars: value.max_line_chars,
        max_output_bytes: value.max_output_bytes,
        timeout: Duration::from_millis(value.timeout_millis.max(1)),
    }
}
fn entry_kind(value: WorkspaceEntryKind) -> WorkspaceWorkerEntryKind {
    match value {
        WorkspaceEntryKind::File => WorkspaceWorkerEntryKind::File,
        WorkspaceEntryKind::Directory => WorkspaceWorkerEntryKind::Directory,
        WorkspaceEntryKind::Symlink => WorkspaceWorkerEntryKind::Symlink,
        WorkspaceEntryKind::Other => WorkspaceWorkerEntryKind::Other,
    }
}
fn error_code(
    error: &sylvander_agent::workspace_executor::WorkspaceExecutorError,
) -> WorkspaceWorkerErrorCode {
    use sylvander_agent::workspace_executor::WorkspaceExecutorError as Error;
    match error {
        Error::ReadOnly(_) => WorkspaceWorkerErrorCode::ReadOnly,
        Error::InvalidPath(_) | Error::PolicyViolation(_) => WorkspaceWorkerErrorCode::PathBoundary,
        Error::WriteConflict(_) => WorkspaceWorkerErrorCode::Conflict,
        Error::Timeout(_) => WorkspaceWorkerErrorCode::Timeout,
        Error::InvalidRequest(_) | Error::ConditionalWriteUnavailable(_) => {
            WorkspaceWorkerErrorCode::InvalidRequest
        }
        Error::Unavailable(_) | Error::Io(_) => WorkspaceWorkerErrorCode::Execution,
    }
}
