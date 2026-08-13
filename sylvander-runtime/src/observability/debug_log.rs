//! Bounded, content-free Runtime lifecycle projection for local governance.
//!
//! Each process owns one capped file. Startup also removes the oldest files
//! from this module's UUID namespace so repeated restarts cannot grow the debug
//! directory without bound. Unrecognized files are never touched.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::{RuntimeEvent, RuntimeToolFailureKind};

pub(crate) const DEBUG_OBSERVATION_LOG_MAX_BYTES: u64 = 16 * 1_024 * 1_024;
pub(crate) const DEBUG_OBSERVATION_LOG_MAX_FILES: usize = 4;
pub(crate) const DEBUG_OBSERVATION_LOG_TOTAL_MAX_BYTES: u64 =
    DEBUG_OBSERVATION_LOG_MAX_BYTES * DEBUG_OBSERVATION_LOG_MAX_FILES as u64;
const DEBUG_DIRECTORY: &str = "debug";
const DEBUG_FILE_PREFIX: &str = "runtime-observations";
const EVENT_OBSERVATIONS_LAGGED: &str = "observations_lagged";
const EVENT_OBSERVATION_LOG_TRUNCATED: &str = "observation_log_truncated";

pub(crate) struct RuntimeObservationDebugLog {
    path: PathBuf,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl RuntimeObservationDebugLog {
    pub(crate) async fn start(
        data_dir: &Path,
        receiver: broadcast::Receiver<RuntimeEvent>,
    ) -> std::io::Result<Self> {
        let directory = data_dir.join(DEBUG_DIRECTORY);
        tokio::fs::create_dir_all(&directory).await?;
        prune_managed_logs(&directory).await?;
        let path = directory.join(format!("{DEBUG_FILE_PREFIX}-{}.jsonl", Uuid::new_v4()));
        let file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task_path = path.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = run(BufWriter::new(file), receiver, shutdown_rx).await {
                tracing::warn!(path = %task_path.display(), %error, "debug observation log stopped");
            }
        });
        Ok(Self {
            path,
            shutdown: Mutex::new(Some(shutdown_tx)),
            task: Mutex::new(Some(task)),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) async fn shutdown(&self) {
        if let Some(sender) = self.shutdown.lock().await.take() {
            let _ = sender.send(());
        }
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }
}

struct ManagedLog {
    path: PathBuf,
    modified: SystemTime,
    bytes: u64,
}

async fn prune_managed_logs(directory: &Path) -> std::io::Result<()> {
    let mut reader = tokio::fs::read_dir(directory).await?;
    let mut logs = Vec::new();
    while let Some(entry) = reader.next_entry().await? {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !managed_log_name(name) {
            continue;
        }
        let metadata = tokio::fs::symlink_metadata(entry.path()).await?;
        logs.push(ManagedLog {
            path: entry.path(),
            modified: metadata.modified()?,
            bytes: metadata.len(),
        });
    }
    logs.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut total = logs.iter().map(|log| log.bytes).sum::<u64>();
    let retained_bytes =
        DEBUG_OBSERVATION_LOG_TOTAL_MAX_BYTES.saturating_sub(DEBUG_OBSERVATION_LOG_MAX_BYTES);
    while logs.len() >= DEBUG_OBSERVATION_LOG_MAX_FILES || total > retained_bytes {
        let oldest = logs.remove(0);
        tokio::fs::remove_file(&oldest.path).await?;
        total = total.saturating_sub(oldest.bytes);
    }
    Ok(())
}

fn managed_log_name(name: &str) -> bool {
    name.strip_prefix(DEBUG_FILE_PREFIX)
        .and_then(|suffix| suffix.strip_prefix('-'))
        .and_then(|suffix| suffix.strip_suffix(".jsonl"))
        .is_some_and(|id| Uuid::parse_str(id).is_ok())
}

async fn run(
    mut writer: BufWriter<tokio::fs::File>,
    mut receiver: broadcast::Receiver<RuntimeEvent>,
    mut shutdown: oneshot::Receiver<()>,
) -> std::io::Result<()> {
    let mut written = 0_u64;
    loop {
        let record = tokio::select! {
            _ = &mut shutdown => {
                while let Ok(event) = receiver.try_recv() {
                    if !write_record(&mut writer, &mut written, event_json(&event)).await? {
                        return writer.flush().await;
                    }
                }
                break;
            },
            next = receiver.recv() => match next {
                Ok(event) => event_json(&event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    json!({"event": EVENT_OBSERVATIONS_LAGGED, "skipped": skipped})
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        };
        if !write_record(&mut writer, &mut written, record).await? {
            break;
        }
    }
    writer.flush().await
}

async fn write_record(
    writer: &mut BufWriter<tokio::fs::File>,
    written: &mut u64,
    record: Value,
) -> std::io::Result<bool> {
    let mut line = serde_json::to_vec(&timestamped(record))?;
    line.push(b'\n');
    if written.saturating_add(line.len() as u64) > DEBUG_OBSERVATION_LOG_MAX_BYTES {
        let mut terminal = serde_json::to_vec(&timestamped(json!({
            "event": EVENT_OBSERVATION_LOG_TRUNCATED,
            "max_bytes": DEBUG_OBSERVATION_LOG_MAX_BYTES,
        })))?;
        terminal.push(b'\n');
        if written.saturating_add(terminal.len() as u64) <= DEBUG_OBSERVATION_LOG_MAX_BYTES {
            writer.write_all(&terminal).await?;
        }
        return Ok(false);
    }
    writer.write_all(&line).await?;
    *written = written.saturating_add(line.len() as u64);
    Ok(true)
}

fn timestamped(mut record: Value) -> Value {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    record["recorded_at_unix_ms"] = json!(timestamp);
    record
}

fn event_json(event: &RuntimeEvent) -> Value {
    let event_name = event.as_str();
    match event {
        RuntimeEvent::ChatAdmitted {
            request_id,
            session_id,
            message_id,
            agent_id,
        } => json!({
            "event": event_name, "request_id": request_id, "session_id": session_id.0,
            "message_id": message_id.0, "agent_id": agent_id.0,
        }),
        RuntimeEvent::ChatDispatchFinished {
            request_id,
            session_id,
            succeeded,
        } => json!({
            "event": event_name, "request_id": request_id,
            "session_id": session_id.0, "succeeded": succeeded,
        }),
        RuntimeEvent::CoordinationTransition {
            session_id,
            outcome,
        } => json!({
            "event": event_name, "session_id": session_id.0,
            "outcome": outcome.as_str(),
        }),
        RuntimeEvent::TurnStarted {
            request_id,
            trace_id,
            turn_id,
            session_id,
            agent_id,
        } => json!({
            "event": event_name, "request_id": request_id, "trace_id": trace_id,
            "turn_id": turn_id, "session_id": session_id.0, "agent_id": agent_id.0,
        }),
        RuntimeEvent::TurnTransitioned {
            turn_id,
            session_id,
            transition,
        } => json!({
            "event": event_name, "turn_id": turn_id, "session_id": session_id.0,
            "sequence": transition.sequence, "iteration": transition.iteration,
            "from": transition.from.as_str(), "to": transition.to.as_str(),
            "reason": transition.reason.as_str(),
            "continuation": transition.continuation
                .map(sylvander_agent::turn::machine::TurnContinuationReason::as_str),
        }),
        RuntimeEvent::ModelRetried {
            turn_id,
            session_id,
            attempt,
        } => json!({
            "event": event_name, "turn_id": turn_id, "session_id": session_id.0,
            "attempt": attempt,
        }),
        RuntimeEvent::ToolStarted {
            turn_id,
            session_id,
            tool_call_id,
            tool_name,
        } => json!({
            "event": event_name, "turn_id": turn_id, "session_id": session_id.0,
            "tool_call_id": tool_call_id, "tool_name": tool_name,
        }),
        RuntimeEvent::ToolFinished {
            turn_id,
            session_id,
            tool_call_id,
            tool_name,
            succeeded,
            failure_kind,
        } => json!({
            "event": event_name, "turn_id": turn_id, "session_id": session_id.0,
            "tool_call_id": tool_call_id, "tool_name": tool_name, "succeeded": succeeded,
            "failure_kind": failure_kind.map(RuntimeToolFailureKind::as_str),
        }),
        RuntimeEvent::ToolRecoveryClassified {
            turn_id,
            session_id,
            tool_call_id,
            position,
            decision,
            operator_action_required,
        } => json!({
            "event": event_name, "turn_id": turn_id, "session_id": session_id.0,
            "tool_call_id": tool_call_id, "position": position, "decision": decision,
            "operator_action_required": operator_action_required,
        }),
        RuntimeEvent::ModelRecoveryClassified {
            turn_id,
            session_id,
            position,
            decision,
            operator_action_required,
        } => json!({
            "event": event_name, "turn_id": turn_id, "session_id": session_id.0,
            "position": position, "decision": decision,
            "operator_action_required": operator_action_required,
        }),
        RuntimeEvent::PersistenceFinished {
            turn_id,
            session_id,
            operation,
            succeeded,
        } => json!({
            "event": event_name, "turn_id": turn_id, "session_id": session_id.0,
            "operation": operation.as_str(), "succeeded": succeeded,
        }),
        RuntimeEvent::TurnCompleted {
            turn_id,
            session_id,
        }
        | RuntimeEvent::TurnInterrupted {
            turn_id,
            session_id,
        } => terminal(event_name, turn_id, session_id),
        RuntimeEvent::TurnFailed {
            turn_id,
            session_id,
            kind,
        } => json!({
            "event": event_name, "turn_id": turn_id, "session_id": session_id.0,
            "failure_kind": kind.as_str(),
        }),
    }
}

fn terminal(
    event: &'static str,
    turn_id: &str,
    session_id: &crate::agent_definition::SessionId,
) -> Value {
    json!({"event": event, "turn_id": turn_id, "session_id": session_id.0})
}
