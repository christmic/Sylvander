//! Unix socket client — line-based JSON over UDS.
//!
//! Mirrors the wire format in `sylvander-channel-unix`. One JSON object
//! per line. The client opens a connection, sends commands, and pushes
//! server events into an mpsc for the main loop to consume.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

use crate::app::ToolInfo;
use crate::event::DomainEvent;

// ===========================================================================
// Wire protocol (mirror of sylvander-channel-unix ServerMsg)
// ===========================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Chat {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    Approve {
        call_id: String,
        approved: bool,
    },
    Answer {
        call_id: String,
        answer: String,
    },
    Interrupt {
        session_id: String,
    },
    ListSessions,
    Ping,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    SessionCreated {
        session_id: String,
    },
    TextDelta {
        session_id: String,
        delta: String,
    },
    ThinkingDelta {
        session_id: String,
        delta: String,
    },
    ToolCall {
        session_id: String,
        #[serde(default)]
        call_id: String,
        tool_name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    ToolResult {
        session_id: String,
        #[serde(default)]
        call_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
    IterationStart {
        session_id: String,
        iteration: u32,
    },
    IterationEnd {
        session_id: String,
        iteration: u32,
        input_tokens: u32,
        output_tokens: u32,
    },
    Done {
        session_id: String,
        text: String,
    },
    Error {
        session_id: String,
        message: String,
    },
    ApprovalRequest {
        session_id: String,
        batch_id: String,
        tools: Vec<ToolInfoMsg>,
    },
    /// Agent forcefully rejected a tool (server-side policy) — surface
    /// the reason in the transcript so the user understands the failure.
    ToolRejected {
        session_id: String,
        tool_name: String,
        reason: String,
    },
    /// Agent asks the user a clarifying question. UX §12.1:
    /// multi_select=false → single choice + free-text fallback;
    /// multi_select=true → multi-select checkboxes + free-text fallback.
    AskUser {
        session_id: String,
        call_id: String,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    },
    TurnInterrupted {
        session_id: String,
        reason: String,
    },
    SessionsList {
        sessions: Vec<SessionInfoMsg>,
    },
    Pong,
}

/// Tools in an ApprovalRequest carry call_id + input (matches `ToolInfo`).
#[derive(Debug, Clone, Deserialize)]
pub struct ToolInfoMsg {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionInfoMsg {
    pub id: String,
    pub label: String,
    pub workspace: String,
    pub last_seen_secs: u64,
}

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
    event_tx: mpsc::UnboundedSender<ClientEvent>,
}

impl UnixClient {
    pub fn new(path: impl Into<PathBuf>) -> (Self, mpsc::UnboundedReceiver<ClientEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        (
            Self {
                path: path.into(),
                writer: None,
                event_tx,
            },
            event_rx,
        )
    }

    /// Try to establish a Unix socket connection. Returns Ok(()) if the
    /// stream was acquired; on failure, the caller can retry later.
    pub async fn connect(&mut self) -> std::io::Result<()> {
        let stream = tokio::net::UnixStream::connect(&self.path).await?;
        let (read, write) = stream.into_split();
        self.writer = Some(write);
        self.spawn_reader(read);
        Ok(())
    }

    /// Spawn the read loop. Each parsed line is forwarded as a Message
    /// event; the loop exits when the socket closes.
    fn spawn_reader(&self, read: OwnedReadHalf) {
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(read).lines();
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<ServerMsg>(line) {
                            Ok(msg) => {
                                if tx.send(ClientEvent::Message(msg)).is_err() {
                                    break;
                                }
                            }
                            Err(_) => {
                                // Drop bad lines silently — server may be
                                // half-started. Real impl could log.
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            let _ = tx.send(ClientEvent::Disconnected);
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
        ServerMsg::SessionCreated { session_id } => DomainEvent::SessionCreated { session_id },
        ServerMsg::TextDelta { delta, .. } => DomainEvent::TextChunk { delta },
        ServerMsg::ThinkingDelta { delta, .. } => DomainEvent::ThinkingChunk { delta },
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
        ServerMsg::ApprovalRequest {
            batch_id, tools, ..
        } => DomainEvent::ApprovalRequested {
            batch_id,
            tools: tools.into_iter().map(Into::into).collect(),
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
        ServerMsg::IterationEnd {
            iteration,
            input_tokens,
            output_tokens,
            ..
        } => DomainEvent::UsageUpdated {
            iteration,
            input_tokens,
            output_tokens,
        },
        // Currently unused by the UI but harmless to receive.
        ServerMsg::IterationStart { .. } | ServerMsg::Pong => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn answer_uses_the_server_supported_wire_shape() {
        let json = serde_json::to_value(ClientMsg::Answer {
            call_id: "c1".into(),
            answer: "blue".into(),
        })
        .unwrap();
        assert_eq!(json["type"], "answer");
        assert_eq!(json["call_id"], "c1");
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
}
