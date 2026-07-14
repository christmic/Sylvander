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
    Message(ServerMsg),
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
        let line = if let Some(line) = handshake_line {
            line
        } else {
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
                                if tx.send(ClientEvent::Message(msg)).await.is_err() {
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
        let writer = match self.writer.as_mut() {
            Some(w) => w,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "socket not connected",
                ));
            }
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
        "model_selection",
        "plans",
        "session_replay",
        "sessions",
        "tasks",
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

/// Translate a parsed server message into a neutral DomainEvent.
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
        ServerMsg::SessionCreated { session_id, .. } => DomainEvent::SessionCreated { session_id },
        ServerMsg::RuntimeInfo {
            model,
            reasoning_effort,
            models,
            permissions,
            capabilities,
            approval_enabled,
            max_attachment_bytes,
            platform,
        } => DomainEvent::RuntimeInfo {
            model,
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
        ServerMsg::Done { text, .. } => DomainEvent::AgentDone { final_text: text },
        ServerMsg::Error { message, .. } => DomainEvent::AgentError { message },
        ServerMsg::TurnInterrupted { reason, .. } => DomainEvent::TurnInterrupted { reason },
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
        | ServerMsg::AgentsDiscovered { .. }
        | ServerMsg::SessionConfig { .. }
        | ServerMsg::FeedbackRecorded { .. }
        | ServerMsg::Pong => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_line_reader_accepts_crlf_and_rejects_oversized_frames() {
        let mut reader = BufReader::new(&b"first\r\nsecond\n"[..]);
        assert_eq!(
            read_bounded_line(&mut reader, 16).await.unwrap(),
            Some("first".into())
        );
        assert_eq!(
            read_bounded_line(&mut reader, 16).await.unwrap(),
            Some("second".into())
        );

        let oversized = vec![b'x'; 17];
        let mut reader = BufReader::new(oversized.as_slice());
        let error = read_bounded_line(&mut reader, 16)
            .await
            .expect_err("oversized frame");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn socket_event_queue_applies_backpressure_at_its_capacity() {
        let (client, _events) = UnixClient::new("/tmp/sylvander-test.sock");
        for index in 0..CLIENT_EVENT_CAPACITY {
            client
                .event_tx
                .try_send(ClientEvent::Diagnostic(index.to_string()))
                .expect("queue slot");
        }
        assert!(matches!(
            client
                .event_tx
                .try_send(ClientEvent::Diagnostic("overflow".into())),
            Err(mpsc::error::TrySendError::Full(_))
        ));
    }

    #[test]
    fn unknown_server_messages_produce_bounded_diagnostics() {
        let line = format!(r#"{{"type":"future_{}"}}"#, "x".repeat(500));
        let diagnostic = parse_server_line(&line).expect_err("unknown event must be visible");
        assert!(diagnostic.starts_with("Rejected server message"));
        assert!(diagnostic.chars().count() < 300);
    }

    #[test]
    fn timeout_wire_event_preserves_recovery_contract() {
        let event = parse_server_msg(ServerMsg::InteractionTimeout {
            session_id: "session-1".into(),
            kind: sylvander_protocol::InteractionTimeoutKind::Tool,
            subject_id: "call-1".into(),
            timeout_secs: 120,
            recovery: sylvander_protocol::TimeoutRecovery::NarrowScope,
        });
        assert!(matches!(
            event,
            Some(DomainEvent::InteractionTimedOut {
                kind: sylvander_protocol::InteractionTimeoutKind::Tool,
                subject_id,
                timeout_secs: 120,
                recovery: sylvander_protocol::TimeoutRecovery::NarrowScope,
            }) if subject_id == "call-1"
        ));
    }

    #[test]
    fn runtime_wire_event_preserves_server_capabilities() {
        let event = parse_server_msg(ServerMsg::RuntimeInfo {
            model: "claude-test".into(),
            reasoning_effort: sylvander_protocol::ReasoningEffort::Medium,
            models: vec![sylvander_protocol::ModelDescriptor {
                id: "claude-test".into(),
                provider: "test".into(),
                capabilities: 0b10001,
                reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Medium],
                lifecycle: sylvander_protocol::ModelLifecycle::Active,
                pricing: None,
            }],
            permissions: sylvander_protocol::PermissionProfile::default(),
            capabilities: 0b10001,
            approval_enabled: true,
            max_attachment_bytes: 4096,
            platform: sylvander_protocol::PlatformSnapshot::default(),
        });
        assert!(matches!(
            event,
            Some(DomainEvent::RuntimeInfo {
                model,
                reasoning_effort: sylvander_protocol::ReasoningEffort::Medium,
                models,
                capabilities: 0b10001,
                approval_enabled: true,
                max_attachment_bytes: 4096,
                ..
            }) if model == "claude-test" && models.len() == 1
        ));
    }

    #[test]
    fn legacy_usage_event_defaults_to_unknown_cost() {
        let message: ServerMsg = serde_json::from_value(serde_json::json!({
            "type": "iteration_end",
            "session_id": "s1",
            "iteration": 1,
            "input_tokens": 10,
            "output_tokens": 2
        }))
        .expect("legacy iteration event");
        assert!(matches!(
            parse_server_msg(message),
            Some(DomainEvent::UsageUpdated {
                cost_nano_usd: None,
                ..
            })
        ));
    }

    #[test]
    fn model_selection_uses_typed_reasoning_effort_on_wire() {
        let value = serde_json::to_value(ClientMsg::SelectModel {
            model: "thinking".into(),
            reasoning_effort: sylvander_protocol::ReasoningEffort::High,
        })
        .unwrap();
        assert_eq!(value["type"], "select_model");
        assert_eq!(value["reasoning_effort"], "high");
    }

    #[test]
    fn permission_selection_is_a_typed_wire_profile() {
        let value = serde_json::to_value(ClientMsg::SelectPermissions {
            profile: sylvander_protocol::PermissionProfile {
                file_access: sylvander_protocol::FileAccess::ReadOnly,
                network_access: sylvander_protocol::NetworkAccess::Denied,
                approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
            },
        })
        .unwrap();
        assert_eq!(value["type"], "select_permissions");
        assert_eq!(value["profile"]["file_access"], "read_only");
        assert_eq!(value["profile"]["approval_policy"], "deny");
    }

    #[test]
    fn context_report_round_trips_as_typed_server_truth() {
        let request = serde_json::to_value(ClientMsg::GetContext {
            session_id: Some("session-1".into()),
        })
        .expect("serialize");
        assert_eq!(request["type"], "get_context");
        assert_eq!(request["session_id"], "session-1");

        let event = parse_server_msg(ServerMsg::ContextReport {
            report: sylvander_protocol::ContextReport {
                model: "deep-code".into(),
                context_window: 100_000,
                used_tokens: 25_000,
                remaining_tokens: 75_000,
                cache_read_tokens: 20_000,
                cache_write_tokens: 1_000,
                sources: vec![],
            },
        });
        assert!(matches!(
            event,
            Some(DomainEvent::ContextReported { report })
                if report.used_tokens == 25_000 && report.cache_read_tokens == 20_000
        ));
    }

    #[test]
    fn compaction_wire_lifecycle_preserves_manual_identity_and_summary() {
        let request = serde_json::to_value(ClientMsg::Compact {
            session_id: "session-1".into(),
        })
        .expect("serialize");
        assert_eq!(request["type"], "compact");
        assert_eq!(request["session_id"], "session-1");

        let event = parse_server_msg(ServerMsg::CompactionCompleted {
            session_id: "session-1".into(),
            report: sylvander_protocol::CompactionReport {
                automatic: false,
                removed_messages: 8,
                condensed_blocks: 0,
                freed_tokens: 2_000,
                summary: Some("preserved summary".into()),
            },
        });
        assert!(matches!(
            event,
            Some(DomainEvent::CompactionCompleted { report })
                if !report.automatic && report.summary.as_deref() == Some("preserved summary")
        ));
    }

    #[test]
    fn operation_errors_do_not_impersonate_agent_failures() {
        let event = parse_server_msg(ServerMsg::OperationError {
            operation: "load_session".into(),
            message: "not found".into(),
        });
        assert!(matches!(
            event,
            Some(DomainEvent::OperationFailed { operation, message })
                if operation == "load_session" && message == "not found"
        ));
    }

    #[test]
    fn boundary_denials_preserve_operation_and_retry_guidance() {
        let event = parse_server_msg(ServerMsg::BoundaryDenied {
            error: sylvander_protocol::BoundaryError {
                code: sylvander_protocol::BoundaryErrorCode::RateLimited,
                operation: "chat".into(),
                request_id: "request-1".into(),
                message: "request rate limit exceeded".into(),
                retry_after_ms: Some(1_500),
            },
        });
        assert!(matches!(
            event,
            Some(DomainEvent::OperationFailed { operation, message })
                if operation == "chat" && message.contains("1500 ms")
        ));
    }

    #[test]
    fn model_retry_wire_event_preserves_backoff_context() {
        let event = parse_server_msg(ServerMsg::ModelRetry {
            session_id: "s1".into(),
            attempt: 2,
            max_attempts: 3,
            delay_ms: 200,
            reason: "rate limited".into(),
            cause: sylvander_protocol::RetryCause::RateLimit,
        });
        assert!(matches!(
            event,
            Some(DomainEvent::ModelRetry {
                attempt: 2,
                max_attempts: 3,
                delay_ms: 200,
                reason,
                cause: sylvander_protocol::RetryCause::RateLimit,
            }) if reason == "rate limited"
        ));
    }

    #[test]
    fn tool_call_adapter_preserves_identity_and_input() {
        let event = parse_server_msg(ServerMsg::ToolCall {
            session_id: "s1".into(),
            call_id: "call-42".into(),
            tool_name: "read".into(),
            input: serde_json::json!({"path": "src/lib.rs"}),
        });
        assert!(matches!(
            event,
            Some(DomainEvent::ToolStarted { call_id, tool_name, input })
                if call_id == "call-42"
                    && tool_name == "read"
                    && input["path"] == "src/lib.rs"
        ));
    }

    #[test]
    fn tool_delta_adapter_preserves_call_identity() {
        let event = parse_server_msg(ServerMsg::ToolOutputDelta {
            session_id: "s1".into(),
            call_id: "call-42".into(),
            tool_name: "read".into(),
            delta: "partial".into(),
        });
        assert!(matches!(
            event,
            Some(DomainEvent::ToolOutputDelta { call_id, tool_name, delta })
                if call_id == "call-42" && tool_name == "read" && delta == "partial"
        ));
    }

    #[test]
    fn answer_uses_the_server_supported_wire_shape() {
        let json = serde_json::to_value(ClientMsg::Answer {
            session_id: "s1".into(),
            call_id: "c1".into(),
            answer: "blue".into(),
        })
        .unwrap();
        assert_eq!(json["type"], "answer");
        assert_eq!(json["call_id"], "c1");
        assert_eq!(json["session_id"], "s1");
    }

    #[test]
    fn approval_rejection_reason_uses_the_typed_wire_shape() {
        let json = serde_json::to_value(ClientMsg::Approve {
            session_id: "s1".into(),
            call_id: "c1".into(),
            approved: false,
            scope: sylvander_protocol::ApprovalScope::Once,
            reason: Some("unsafe outside workspace".into()),
        })
        .unwrap();
        assert_eq!(json["type"], "approve");
        assert_eq!(json["call_id"], "c1");
        assert_eq!(json["reason"], "unsafe outside workspace");
    }

    #[test]
    fn interrupt_is_scoped_to_one_session_on_the_wire() {
        let json = serde_json::to_value(ClientMsg::Interrupt {
            session_id: "session-7".into(),
        })
        .unwrap();
        assert_eq!(json["type"], "interrupt");
        assert_eq!(json["session_id"], "session-7");
    }

    #[test]
    fn interrupted_wire_event_has_a_terminal_domain_state() {
        let event = parse_server_msg(ServerMsg::TurnInterrupted {
            session_id: "session-7".into(),
            reason: "interrupted by user".into(),
        });
        assert!(matches!(
            event,
            Some(DomainEvent::TurnInterrupted { reason })
                if reason == "interrupted by user"
        ));
    }

    #[test]
    fn plan_wire_event_maps_to_review_and_resolution_is_typed() {
        let event = parse_server_msg(ServerMsg::PlanProposed {
            session_id: "s1".into(),
            plan_id: "plan-1".into(),
            steps: vec!["inspect".into(), "verify".into()],
            current: 1,
        });
        assert!(matches!(
            event,
            Some(DomainEvent::PlanReceived { plan_id, current: 1, .. })
                if plan_id == "plan-1"
        ));

        let json = serde_json::to_value(ClientMsg::ResolvePlan {
            session_id: "s1".into(),
            plan_id: "plan-1".into(),
            decision: sylvander_protocol::PlanDecision::Approved,
        })
        .expect("serialize");
        assert_eq!(json["type"], "resolve_plan");
        assert_eq!(json["decision"]["decision"], "approved");

        let update = parse_server_msg(ServerMsg::PlanUpdated {
            session_id: "s1".into(),
            plan_id: "plan-1".into(),
            steps: vec!["inspect".into(), "verify".into()],
            current: 1,
        });
        assert!(matches!(
            update,
            Some(DomainEvent::PlanUpdated { current: 1, .. })
        ));
    }

    #[test]
    fn background_task_lifecycle_and_scoped_cancel_keep_identity() {
        let event = parse_server_msg(ServerMsg::TaskCompleted {
            session_id: "s1".into(),
            task_id: "task-42".into(),
            summary: "found it".into(),
        });
        assert!(matches!(
            event,
            Some(DomainEvent::TaskCompleted { task_id, summary })
                if task_id == "task-42" && summary == "found it"
        ));

        let json = serde_json::to_value(ClientMsg::CancelTask {
            session_id: "s1".into(),
            task_id: "task-42".into(),
        })
        .expect("serialize");
        assert_eq!(json["type"], "cancel_task");
        assert_eq!(json["session_id"], "s1");
        assert_eq!(json["task_id"], "task-42");
    }

    #[test]
    fn chat_serializes_typed_attachments_without_text_wrappers() {
        let message = ClientMsg::Chat {
            text: "review".into(),
            attachments: vec![sylvander_protocol::MessageAttachment {
                id: "a1".into(),
                kind: sylvander_protocol::AttachmentKind::File,
                name: "src/main.rs".into(),
                mime_type: "text/x-rust".into(),
                content: sylvander_protocol::AttachmentContent::Text {
                    text: "fn main() {}".into(),
                },
                byte_count: 12,
            }],
            session_id: Some("s1".into()),
            workspace: Some("/repo".into()),
        };
        let value = serde_json::to_value(message).expect("serialize");
        assert_eq!(value["attachments"][0]["name"], "src/main.rs");
        assert!(!value["text"].as_str().unwrap().contains("[attachments]"));
    }

    #[test]
    fn persisted_history_maps_to_protocol_neutral_roles() {
        let event = parse_server_msg(ServerMsg::SessionHistory {
            session: SessionInfoMsg {
                id: "s1".into(),
                label: "Auth work".into(),
                workspace: "/workspace".into(),
                last_seen_secs: 3,
            },
            messages: vec![
                HistoryMessageMsg {
                    role: "user".into(),
                    text: "hello".into(),
                },
                HistoryMessageMsg {
                    role: "assistant".into(),
                    text: "hi".into(),
                },
            ],
            iterations: 2,
            input_tokens: 120,
            output_tokens: 30,
            cost_nano_usd: Some(45_000),
            notice: None,
            source_session_id: None,
            recovery: false,
            replay_truncated: false,
        });
        assert!(matches!(
            event,
            Some(DomainEvent::SessionHistoryLoaded {
                session,
                messages,
                iterations: 2,
                input_tokens: 120,
                output_tokens: 30,
                cost_nano_usd: Some(45_000),
                notice: None,
                source_session_id: None,
                recovery: false,
                replay_truncated: false,
            })
                if session.id == "s1"
                    && messages[0].role == crate::model::HistoryRole::User
                    && messages[1].role == crate::model::HistoryRole::Assistant
        ));
    }
}
