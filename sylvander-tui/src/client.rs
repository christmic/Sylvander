//! Unix socket client — line-based JSON over UDS.
//!
//! Mirrors the wire format in `sylvander-channel-unix`. One JSON object
//! per line. The client opens a connection, sends commands, and pushes
//! server events into an mpsc for the main loop to consume.

use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

use crate::app::ToolInfo;
use crate::event::DomainEvent;

pub use sylvander_protocol::UiClientMessage as ClientMsg;
pub use sylvander_protocol::{
    UiHistoryMessage as HistoryMessageMsg, UiServerMessage as ServerMsg,
    UiSessionInfo as SessionInfoMsg, UiToolInfo as ToolInfoMsg,
};

const CLIENT_EVENT_CAPACITY: usize = 1_024;
const MAX_SERVER_LINE_BYTES: usize = 8 * 1024 * 1024;

// ===========================================================================
// Wire protocol (mirror of sylvander-channel-unix ServerMsg)
// ===========================================================================

impl From<ToolInfoMsg> for ToolInfo {
    fn from(t: ToolInfoMsg) -> Self {
        ToolInfo {
            call_id: t.call_id,
            tool_name: t.tool_name,
            input: t.input,
        }
    }
}

// ===========================================================================
// Event surfaced to AppState
// ===========================================================================

/// High-level event for the main loop to apply to `AppState`.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Socket just disconnected — switch status to Disconnected,
    /// surface an Info message, drop session.
    Disconnected,
    /// A parsed server message arrived.
    Message(Box<ServerMsg>),
    Diagnostic(String),
}

// ===========================================================================
// UnixClient
// ===========================================================================

/// A single Unix socket connection.
///
/// Holds the writer half directly (cheap to clone) so the TUI can fire
/// messages from any code path. The reader runs in a background task and
/// pushes `ClientEvent`s into the event channel.
pub struct UnixClient {
    path: PathBuf,
    writer: Option<OwnedWriteHalf>,
    /// Notified by the reader task when the connection ends so the main
    /// loop can flip the UI to Disconnected without polling.
    event_tx: mpsc::Sender<ClientEvent>,
}

impl UnixClient {
    pub fn new(path: impl Into<PathBuf>) -> (Self, mpsc::Receiver<ClientEvent>) {
        let (event_tx, event_rx) = mpsc::channel(CLIENT_EVENT_CAPACITY);
        (
            Self {
                path: path.into(),
                writer: None,
                event_tx,
            },
            event_rx,
        )
    }

    /// Establish a Unix socket connection and negotiate the UI protocol.
    pub async fn connect(&mut self) -> std::io::Result<sylvander_protocol::UiProtocolWelcome> {
        let stream = tokio::net::UnixStream::connect(&self.path).await?;
        let (read, mut write) = stream.into_split();
        let hello = ClientMsg::Hello {
            protocol: sylvander_protocol::UiProtocolHello {
                client_name: "sylvander-tui".into(),
                min_version: sylvander_protocol::UI_PROTOCOL_MIN_VERSION,
                max_version: sylvander_protocol::UI_PROTOCOL_MAX_VERSION,
                capabilities: tui_protocol_capabilities(),
            },
        };
        let encoded = serde_json::to_string(&hello)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        write.write_all(encoded.as_bytes()).await?;
        write.write_all(b"\n").await?;
        write.flush().await?;

        let mut reader = BufReader::new(read);
        let handshake_line = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            read_bounded_line(&mut reader, MAX_SERVER_LINE_BYTES),
        )
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "protocol handshake timed out")
        })??;
        let Some(line) = handshake_line else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "server closed during protocol handshake",
            ));
        };
        let welcome = match serde_json::from_str::<ServerMsg>(&line) {
            Ok(ServerMsg::Welcome { protocol }) => protocol,
            Ok(ServerMsg::ProtocolError { error }) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("protocol {}: {}", error.code, error.message),
                ));
            }
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "server did not acknowledge protocol handshake",
                ));
            }
            Err(error) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid protocol handshake: {error}"),
                ));
            }
        };
        if !(sylvander_protocol::UI_PROTOCOL_MIN_VERSION
            ..=sylvander_protocol::UI_PROTOCOL_MAX_VERSION)
            .contains(&welcome.version)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("server selected unsupported protocol v{}", welcome.version),
            ));
        }
        self.writer = Some(write);
        self.spawn_reader(reader);
        Ok(welcome)
    }

    /// Spawn the read loop. Each parsed line is forwarded as a Message
    /// event; the loop exits when the socket closes.
    fn spawn_reader(&self, reader: BufReader<OwnedReadHalf>) {
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let mut reader = reader;
            loop {
                match read_bounded_line(&mut reader, MAX_SERVER_LINE_BYTES).await {
                    Ok(Some(line)) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        match parse_server_line(line) {
                            Ok(msg) => {
                                if tx.send(ClientEvent::Message(Box::new(msg))).await.is_err() {
                                    break;
                                }
                            }
                            Err(diagnostic) => {
                                if tx.send(ClientEvent::Diagnostic(diagnostic)).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = tx
                            .send(ClientEvent::Diagnostic(format!(
                                "Rejected server stream: {error}"
                            )))
                            .await;
                        break;
                    }
                }
            }
            let _ = tx.send(ClientEvent::Disconnected).await;
        });
    }

    /// Send a client message. Returns Err if not connected or write fails.
    pub async fn send(&mut self, msg: &ClientMsg) -> std::io::Result<()> {
        let Some(writer) = self.writer.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "socket not connected",
            ));
        };
        let json = serde_json::to_string(msg)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writer.write_all(json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.writer.is_some()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

async fn read_bounded_line<R>(reader: &mut R, limit: usize) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::with_capacity(4 * 1024);
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(take) > limit {
            reader.consume(take);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("server message exceeds {} MiB limit", limit / 1024 / 1024),
            ));
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            break;
        }
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map(Some).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("server message is not valid UTF-8: {error}"),
        )
    })
}

fn tui_protocol_capabilities() -> Vec<String> {
    [
        "attachments",
        "approval_scopes",
        "compaction",
        "diagnostics",
        sylvander_protocol::FEEDBACK_CAPABILITY,
        sylvander_protocol::MEMORY_CONFIRMATION_CAPABILITY,
        "model_selection",
        "plans",
        "session_replay",
        "sessions",
        "tasks",
        sylvander_protocol::USER_PROFILE_CAPABILITY,
        "workspace_rollback",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn bounded_diagnostic(message: &str) -> String {
    message.chars().take(240).collect()
}

fn parse_server_line(line: &str) -> Result<ServerMsg, String> {
    serde_json::from_str(line).map_err(|error| {
        format!(
            "Rejected server message ({} bytes): {}",
            line.len(),
            bounded_diagnostic(&error.to_string())
        )
    })
}

// ===========================================================================
// Protocol → Domain adapter (only place that knows both worlds)
// ===========================================================================

/// Translate a parsed server message into a neutral `DomainEvent`.
///
/// This is the ONLY function that bridges wire-format and domain state.
/// Replace it when adding a new transport (WebSocket, HTTP, ...) — no
/// other file needs to change.
pub fn parse_server_msg(msg: ServerMsg) -> Option<DomainEvent> {
    Some(match msg {
        ServerMsg::Welcome { protocol } => DomainEvent::ProtocolDiagnostic {
            message: format!(
                "unexpected repeated welcome for protocol v{}",
                protocol.version
            ),
        },
        ServerMsg::ProtocolError { error } => DomainEvent::ProtocolDiagnostic {
            message: format!("{}: {}", error.code, error.message),
        },
        ServerMsg::SessionCreated { session_id, config } => {
            DomainEvent::SessionCreated { session_id, config }
        }
        ServerMsg::AgentsDiscovered { agents } => DomainEvent::AgentsDiscovered { agents },
        ServerMsg::SessionConfig { state } => DomainEvent::SessionConfigLoaded { state },
        ServerMsg::MemoryConfirmation { response } => {
            let version = match &response {
                sylvander_protocol::MemoryConfirmationResponse::Pending { version, .. }
                | sylvander_protocol::MemoryConfirmationResponse::Recorded { version, .. }
                | sylvander_protocol::MemoryConfirmationResponse::Error { version, .. } => *version,
            };
            if version == sylvander_protocol::MEMORY_CONFIRMATION_PROTOCOL_VERSION {
                match response {
                    sylvander_protocol::MemoryConfirmationResponse::Pending {
                        session_id,
                        confirmations,
                        ..
                    } => DomainEvent::MemoryConfirmationsLoaded {
                        session_id,
                        confirmations,
                    },
                    sylvander_protocol::MemoryConfirmationResponse::Recorded {
                        candidate_id,
                        decision,
                        ..
                    } => DomainEvent::MemoryConfirmationRecorded {
                        candidate_id,
                        decision,
                    },
                    sylvander_protocol::MemoryConfirmationResponse::Error { message, .. } => {
                        DomainEvent::MemoryConfirmationFailed { message }
                    }
                }
            } else {
                DomainEvent::ProtocolDiagnostic {
                    message: format!(
                        "memory confirmation protocol v{version} rejected; expected v{}",
                        sylvander_protocol::MEMORY_CONFIRMATION_PROTOCOL_VERSION
                    ),
                }
            }
        }
        ServerMsg::RuntimeInfo {
            model,
            reasoning_effort,
            models,
            permissions,
            capabilities,
            approval_enabled,
            max_attachment_bytes,
            platform,
            ..
        } => DomainEvent::RuntimeInfo {
            model: model.model_id,
            reasoning_effort,
            models,
            permissions,
            capabilities,
            approval_enabled,
            max_attachment_bytes,
            platform,
        },
        ServerMsg::ContextReport { report } => DomainEvent::ContextReported { report },
        ServerMsg::CompactionStarted { automatic, .. } => {
            DomainEvent::CompactionStarted { automatic }
        }
        ServerMsg::CompactionCompleted { report, .. } => {
            DomainEvent::CompactionCompleted { report }
        }
        ServerMsg::CompactionFailed {
            automatic, reason, ..
        } => DomainEvent::CompactionFailed { automatic, reason },
        ServerMsg::WorkspaceRollbackPreview {
            session_id,
            preview,
        } => DomainEvent::WorkspaceRollbackPreviewed {
            session_id,
            preview,
        },
        ServerMsg::WorkspaceRollbackCompleted { report, .. } => {
            DomainEvent::WorkspaceRollbackCompleted { report }
        }
        ServerMsg::WorkspaceRollbackFailed { reason, .. } => {
            DomainEvent::WorkspaceRollbackFailed { reason }
        }
        ServerMsg::CodingSessionDiff { diff, .. } => DomainEvent::CodingSessionDiffLoaded {
            status: diff.status,
            patch: diff.patch,
        },
        ServerMsg::CodingSessionAccepted { .. } => DomainEvent::CodingSessionAccepted,
        ServerMsg::CodingSessionDiscarded { .. } => DomainEvent::CodingSessionDiscarded,
        ServerMsg::CodingSessionOperationFailed {
            operation, reason, ..
        } => DomainEvent::CodingSessionOperationFailed { operation, reason },
        ServerMsg::TextDelta { delta, .. } => DomainEvent::TextChunk { delta },
        ServerMsg::ThinkingDelta { delta, .. } => DomainEvent::ThinkingChunk { delta },
        ServerMsg::ModelRetry {
            attempt,
            max_attempts,
            delay_ms,
            reason,
            cause,
            ..
        } => DomainEvent::ModelRetry {
            attempt,
            max_attempts,
            delay_ms,
            reason,
            cause,
        },
        ServerMsg::InteractionTimeout {
            kind,
            subject_id,
            timeout_secs,
            recovery,
            ..
        } => DomainEvent::InteractionTimedOut {
            kind,
            subject_id,
            timeout_secs,
            recovery,
        },
        ServerMsg::ToolCall {
            call_id,
            tool_name,
            input,
            ..
        } => DomainEvent::ToolStarted {
            call_id,
            tool_name,
            input,
        },
        ServerMsg::ToolOutputDelta {
            call_id,
            tool_name,
            delta,
            ..
        } => DomainEvent::ToolOutputDelta {
            call_id,
            tool_name,
            delta,
        },
        ServerMsg::ToolResult {
            call_id,
            tool_name,
            output,
            is_error,
            ..
        } => DomainEvent::ToolFinished {
            call_id,
            tool_name,
            output,
            is_error,
        },
        ServerMsg::Done {
            text,
            feedback_target,
            ..
        } => DomainEvent::AgentDone {
            final_text: text,
            feedback_target,
        },
        ServerMsg::Error {
            message,
            feedback_target,
            ..
        } => DomainEvent::AgentError {
            message,
            feedback_target,
        },
        ServerMsg::TurnInterrupted {
            reason,
            feedback_target,
            ..
        } => DomainEvent::TurnInterrupted {
            reason,
            feedback_target,
        },
        ServerMsg::PlanProposed {
            plan_id,
            steps,
            current,
            ..
        } => DomainEvent::PlanReceived {
            plan_id,
            steps,
            current,
        },
        ServerMsg::PlanUpdated {
            plan_id,
            steps,
            current,
            ..
        } => DomainEvent::PlanUpdated {
            plan_id,
            steps,
            current,
        },
        ServerMsg::TaskStarted {
            task_id,
            owner,
            purpose,
            ..
        } => DomainEvent::TaskStarted {
            task_id,
            owner,
            purpose,
        },
        ServerMsg::TaskProgress {
            task_id, message, ..
        } => DomainEvent::TaskProgress { task_id, message },
        ServerMsg::TaskCompleted {
            task_id, summary, ..
        } => DomainEvent::TaskCompleted { task_id, summary },
        ServerMsg::TaskFailed { task_id, error, .. } => DomainEvent::TaskFailed { task_id, error },
        ServerMsg::TaskCancelled {
            task_id, reason, ..
        } => DomainEvent::TaskCancelled { task_id, reason },
        ServerMsg::ApprovalRequest {
            batch_id,
            tools,
            allowed_scopes,
            ..
        } => DomainEvent::ApprovalRequested {
            batch_id,
            tools: tools.into_iter().map(Into::into).collect(),
            allowed_scopes,
        },
        ServerMsg::AskUser {
            call_id,
            question,
            options,
            multi_select,
            ..
        } => DomainEvent::AskUserRequested {
            call_id,
            question,
            options,
            multi_select,
        },
        ServerMsg::ToolRejected {
            tool_name, reason, ..
        } => DomainEvent::ToolRejected { tool_name, reason },
        ServerMsg::SessionsList { sessions } => DomainEvent::SessionsLoaded {
            sessions: sessions
                .into_iter()
                .map(|session| crate::model::SessionSummary {
                    id: session.id,
                    label: session.label,
                    workspace: session.workspace,
                    last_seen_secs: session.last_seen_secs,
                })
                .collect(),
        },
        ServerMsg::SessionHistory {
            session,
            messages,
            iterations,
            input_tokens,
            output_tokens,
            cost_nano_usd,
            notice,
            source_session_id,
            recovery,
            replay_truncated,
        } => DomainEvent::SessionHistoryLoaded {
            session: crate::model::SessionSummary {
                id: session.id,
                label: session.label,
                workspace: session.workspace,
                last_seen_secs: session.last_seen_secs,
            },
            messages: messages
                .into_iter()
                .map(|message| crate::model::HistoryEntry {
                    role: match message.role.as_str() {
                        "user" => crate::model::HistoryRole::User,
                        "assistant" => crate::model::HistoryRole::Assistant,
                        _ => crate::model::HistoryRole::Tool,
                    },
                    text: message.text,
                })
                .collect(),
            iterations,
            input_tokens,
            output_tokens,
            cost_nano_usd,
            notice,
            source_session_id,
            recovery,
            replay_truncated,
        },
        ServerMsg::SessionUpdated {
            session_id,
            label,
            archived,
        } => DomainEvent::SessionUpdated {
            session_id,
            label,
            archived,
        },
        ServerMsg::SessionDeleted { session_id } => DomainEvent::SessionDeleted { session_id },
        ServerMsg::UserProfile { response } => DomainEvent::UserProfileReceived { response },
        ServerMsg::FeedbackRecorded { feedback_id } => {
            DomainEvent::FeedbackRecorded { feedback_id }
        }
        ServerMsg::OperationError { operation, message } => {
            DomainEvent::OperationFailed { operation, message }
        }
        ServerMsg::BoundaryDenied { error } => DomainEvent::OperationFailed {
            operation: error.operation,
            message: match error.retry_after_ms {
                Some(delay) => format!("{} (retry after {delay} ms)", error.message),
                None => error.message,
            },
        },
        ServerMsg::IterationEnd {
            iteration,
            input_tokens,
            output_tokens,
            cost_nano_usd,
            ..
        } => DomainEvent::UsageUpdated {
            iteration,
            input_tokens: input_tokens.into(),
            output_tokens: output_tokens.into(),
            cost_nano_usd,
        },
        // Currently unused by the UI but harmless to receive.
        ServerMsg::IterationStart { .. }
        | ServerMsg::AgentAdmin { .. }
        | ServerMsg::RegistryAdmin { .. }
        | ServerMsg::IdentityBinding { .. }
        | ServerMsg::Pong => return None,
    })
}

#[cfg(test)]
#[path = "../tests/unit/client.rs"]
mod tests;
