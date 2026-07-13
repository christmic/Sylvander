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
        #[serde(skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<sylvander_protocol::MessageAttachment>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    Approve {
        call_id: String,
        approved: bool,
        scope: sylvander_protocol::ApprovalScope,
    },
    Answer {
        call_id: String,
        answer: String,
    },
    Interrupt {
        session_id: String,
    },
    ResolvePlan {
        plan_id: String,
        decision: sylvander_protocol::PlanDecision,
    },
    CancelTask {
        session_id: String,
        task_id: String,
    },
    ListSessions,
    LoadSession {
        session_id: String,
    },
    RenameSession {
        session_id: String,
        label: String,
    },
    ArchiveSession {
        session_id: String,
    },
    RestoreSession {
        session_id: String,
    },
    DeleteSession {
        session_id: String,
    },
    ForkSession {
        session_id: String,
    },
    GetRuntimeInfo,
    GetContext {
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    Compact {
        session_id: String,
    },
    SelectModel {
        model: String,
        reasoning_effort: sylvander_protocol::ReasoningEffort,
    },
    SelectPermissions {
        profile: sylvander_protocol::PermissionProfile,
    },
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
    ModelRetry {
        session_id: String,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        reason: String,
    },
    ToolCall {
        session_id: String,
        #[serde(default)]
        call_id: String,
        tool_name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    ToolOutputDelta {
        session_id: String,
        #[serde(default)]
        call_id: String,
        tool_name: String,
        delta: String,
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
        #[serde(default = "default_approval_scopes")]
        allowed_scopes: Vec<sylvander_protocol::ApprovalScope>,
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
    PlanProposed {
        session_id: String,
        plan_id: String,
        steps: Vec<String>,
        current: usize,
    },
    PlanUpdated {
        session_id: String,
        plan_id: String,
        steps: Vec<String>,
        current: usize,
    },
    TaskStarted {
        session_id: String,
        task_id: String,
        owner: String,
        purpose: String,
    },
    TaskProgress {
        session_id: String,
        task_id: String,
        message: String,
    },
    TaskCompleted {
        session_id: String,
        task_id: String,
        summary: String,
    },
    TaskFailed {
        session_id: String,
        task_id: String,
        error: String,
    },
    TaskCancelled {
        session_id: String,
        task_id: String,
        reason: String,
    },
    SessionsList {
        sessions: Vec<SessionInfoMsg>,
    },
    SessionHistory {
        session: SessionInfoMsg,
        messages: Vec<HistoryMessageMsg>,
        iterations: u32,
        input_tokens: u64,
        output_tokens: u64,
    },
    SessionUpdated {
        session_id: String,
        label: Option<String>,
        archived: bool,
    },
    SessionDeleted {
        session_id: String,
    },
    RuntimeInfo {
        model: String,
        #[serde(default)]
        reasoning_effort: sylvander_protocol::ReasoningEffort,
        #[serde(default)]
        models: Vec<sylvander_protocol::ModelDescriptor>,
        #[serde(default)]
        permissions: sylvander_protocol::PermissionProfile,
        capabilities: u8,
        approval_enabled: bool,
        max_attachment_bytes: usize,
    },
    ContextReport {
        report: sylvander_protocol::ContextReport,
    },
    CompactionStarted {
        session_id: String,
        automatic: bool,
    },
    CompactionCompleted {
        session_id: String,
        report: sylvander_protocol::CompactionReport,
    },
    CompactionFailed {
        session_id: String,
        automatic: bool,
        reason: String,
    },
    OperationError {
        operation: String,
        message: String,
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

#[derive(Debug, Clone, Deserialize)]
pub struct HistoryMessageMsg {
    pub role: String,
    pub text: String,
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
        ServerMsg::RuntimeInfo {
            model,
            reasoning_effort,
            models,
            permissions,
            capabilities,
            approval_enabled,
            max_attachment_bytes,
        } => DomainEvent::RuntimeInfo {
            model,
            reasoning_effort,
            models,
            permissions,
            capabilities,
            approval_enabled,
            max_attachment_bytes,
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
        ServerMsg::TextDelta { delta, .. } => DomainEvent::TextChunk { delta },
        ServerMsg::ThinkingDelta { delta, .. } => DomainEvent::ThinkingChunk { delta },
        ServerMsg::ModelRetry {
            attempt,
            max_attempts,
            delay_ms,
            reason,
            ..
        } => DomainEvent::ModelRetry {
            attempt,
            max_attempts,
            delay_ms,
            reason,
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
        ServerMsg::IterationEnd {
            iteration,
            input_tokens,
            output_tokens,
            ..
        } => DomainEvent::UsageUpdated {
            iteration,
            input_tokens: input_tokens.into(),
            output_tokens: output_tokens.into(),
        },
        // Currently unused by the UI but harmless to receive.
        ServerMsg::IterationStart { .. } | ServerMsg::Pong => return None,
    })
}

fn default_approval_scopes() -> Vec<sylvander_protocol::ApprovalScope> {
    vec![sylvander_protocol::ApprovalScope::Once]
}

#[cfg(test)]
mod tests {
    use super::*;

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
            }],
            permissions: sylvander_protocol::PermissionProfile::default(),
            capabilities: 0b10001,
            approval_enabled: true,
            max_attachment_bytes: 4096,
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
    fn model_retry_wire_event_preserves_backoff_context() {
        let event = parse_server_msg(ServerMsg::ModelRetry {
            session_id: "s1".into(),
            attempt: 2,
            max_attempts: 3,
            delay_ms: 200,
            reason: "rate limited".into(),
        });
        assert!(matches!(
            event,
            Some(DomainEvent::ModelRetry {
                attempt: 2,
                max_attempts: 3,
                delay_ms: 200,
                reason,
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
        });
        assert!(matches!(
            event,
            Some(DomainEvent::SessionHistoryLoaded {
                session,
                messages,
                iterations: 2,
                input_tokens: 120,
                output_tokens: 30,
            })
                if session.id == "s1"
                    && messages[0].role == crate::model::HistoryRole::User
                    && messages[1].role == crate::model::HistoryRole::Assistant
        ));
    }
}
