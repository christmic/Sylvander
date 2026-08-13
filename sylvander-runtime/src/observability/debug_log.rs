use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::{
    RuntimeEvent, RuntimeFailureKind, RuntimePersistenceOperation, RuntimeToolFailureKind,
};

pub(crate) const DEBUG_OBSERVATION_LOG_MAX_BYTES: u64 = 16 * 1_024 * 1_024;
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
    match event {
        RuntimeEvent::ChatAdmitted {
            request_id,
            session_id,
            message_id,
            agent_id,
        } => json!({
            "event": "chat_admitted", "request_id": request_id, "session_id": session_id.0,
            "message_id": message_id.0, "agent_id": agent_id.0,
        }),
        RuntimeEvent::ChatDispatchFinished {
            request_id,
            session_id,
            succeeded,
        } => json!({
            "event": "chat_dispatch_finished", "request_id": request_id,
            "session_id": session_id.0, "succeeded": succeeded,
        }),
        RuntimeEvent::TurnStarted {
            request_id,
            trace_id,
            turn_id,
            session_id,
            agent_id,
        } => json!({
            "event": "turn_started", "request_id": request_id, "trace_id": trace_id,
            "turn_id": turn_id, "session_id": session_id.0, "agent_id": agent_id.0,
        }),
        RuntimeEvent::TurnTransitioned {
            turn_id,
            session_id,
            transition,
        } => json!({
            "event": "turn_transitioned", "turn_id": turn_id, "session_id": session_id.0,
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
            "event": "model_retried", "turn_id": turn_id, "session_id": session_id.0,
            "attempt": attempt,
        }),
        RuntimeEvent::ToolStarted {
            turn_id,
            session_id,
            tool_call_id,
            tool_name,
        } => json!({
            "event": "tool_started", "turn_id": turn_id, "session_id": session_id.0,
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
            "event": "tool_finished", "turn_id": turn_id, "session_id": session_id.0,
            "tool_call_id": tool_call_id, "tool_name": tool_name, "succeeded": succeeded,
            "failure_kind": failure_kind.map(tool_failure_kind),
        }),
        RuntimeEvent::PersistenceFinished {
            turn_id,
            session_id,
            operation,
            succeeded,
        } => json!({
            "event": "persistence_finished", "turn_id": turn_id, "session_id": session_id.0,
            "operation": persistence_operation(*operation), "succeeded": succeeded,
        }),
        RuntimeEvent::TurnCompleted {
            turn_id,
            session_id,
        } => terminal("turn_completed", turn_id, session_id),
        RuntimeEvent::TurnInterrupted {
            turn_id,
            session_id,
        } => terminal("turn_interrupted", turn_id, session_id),
        RuntimeEvent::TurnFailed {
            turn_id,
            session_id,
            kind,
        } => json!({
            "event": "turn_failed", "turn_id": turn_id, "session_id": session_id.0,
            "failure_kind": failure_kind(*kind),
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

const fn failure_kind(kind: RuntimeFailureKind) -> &'static str {
    match kind {
        RuntimeFailureKind::UnknownSession => "unknown_session",
        RuntimeFailureKind::Authentication => "authentication",
        RuntimeFailureKind::AgentLoop => "agent_loop",
        RuntimeFailureKind::Configuration => "configuration",
        RuntimeFailureKind::Persistence => "persistence",
    }
}

const fn tool_failure_kind(kind: RuntimeToolFailureKind) -> &'static str {
    match kind {
        RuntimeToolFailureKind::FilesystemBoundaryPolicyViolation => {
            "filesystem_boundary_policy_violation"
        }
    }
}

const fn persistence_operation(operation: RuntimePersistenceOperation) -> &'static str {
    match operation {
        RuntimePersistenceOperation::InspectSession => "inspect_session",
        RuntimePersistenceOperation::CreateSession => "create_session",
        RuntimePersistenceOperation::RestoreHistory => "restore_history",
        RuntimePersistenceOperation::BeginTurn => "begin_turn",
        RuntimePersistenceOperation::BeginToolCall => "begin_tool_call",
        RuntimePersistenceOperation::FinishToolCall => "finish_tool_call",
        RuntimePersistenceOperation::RecordUsage => "record_usage",
        RuntimePersistenceOperation::CompleteTurn => "complete_turn",
        RuntimePersistenceOperation::FinishTurn => "finish_turn",
        RuntimePersistenceOperation::ReplaceHistory => "replace_history",
    }
}
