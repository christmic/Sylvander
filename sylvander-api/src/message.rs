//! Versioned message envelopes and transient Runtime event DTOs.
//!
//! These values cross the Runtime/Channel boundary and are serializable.
//! Delivery, subscription, and backpressure behavior are application-layer
//! concerns and deliberately remain outside this module.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AgentId, ApprovalScope, CompactionReport, InteractionTimeoutKind, RetryCause, SessionId,
    SessionMetadata, TimeoutRecovery,
};

// ===========================================================================
// Message envelope types
// ===========================================================================

/// Unique identifier for a bus message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MessageId(pub Uuid);

impl MessageId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

/// Who sent the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Sender {
    User(String),
    Agent(AgentId),
    System,
}

/// Who should receive the message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Recipient {
    Agent(AgentId),
    Broadcast,
}

/// Agent lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum AgentStatus {
    Starting,
    Running,
    Idle,
    Stopped,
}

// ===========================================================================
// StreamEvent — the core event protocol
// ===========================================================================

/// Streaming events published during agent loop execution.
///
/// These are transient — not stored in session history.
/// Only `Done` triggers a history write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Runtime admitted the turn and completed all required pre-execution
    /// persistence and composition. This is not emitted for a rejected turn.
    TurnStarted {
        /// Opaque Runtime correlation identity for this turn.
        turn_id: String,
    },
    TextDelta {
        delta: String,
    },
    ThinkingDelta {
        delta: String,
    },
    ModelRetry {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        reason: String,
        cause: RetryCause,
    },
    InteractionTimedOut {
        kind: InteractionTimeoutKind,
        subject_id: String,
        timeout_secs: u64,
        recovery: TimeoutRecovery,
    },
    CompactionStarted {
        automatic: bool,
    },
    CompactionCompleted {
        report: CompactionReport,
    },
    CompactionFailed {
        automatic: bool,
        reason: String,
    },
    ToolCall {
        call_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    ToolOutputDelta {
        call_id: String,
        tool_name: String,
        delta: String,
    },
    ToolResult {
        call_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
    IterationStart {
        iteration: u32,
    },
    IterationEnd {
        iteration: u32,
        input_tokens: u32,
        output_tokens: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_nano_usd: Option<u64>,
    },
    Done {
        text: String,
    },
    /// The active turn failed and will emit no later terminal event.
    Error {
        message: String,
    },
    ToolApprovalRequired {
        batch_id: String,
        tools: Vec<ToolCallInfo>,
        /// Scopes the operator permits for this request. `Once` is always
        /// present; persistent approval is never implied by the UI.
        allowed_scopes: Vec<ApprovalScope>,
    },
    AskUser {
        call_id: String,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    },
    UserAnswer {
        call_id: String,
        answer: Vec<String>,
    },
    /// The active turn for this session was cancelled by its user. This is a
    /// turn terminal event; it does not stop the Agent or discard the session.
    TurnInterrupted {
        reason: String,
    },
    PlanProposed {
        plan_id: String,
        steps: Vec<String>,
        current: usize,
    },
    PlanUpdated {
        plan_id: String,
        steps: Vec<String>,
        current: usize,
    },
    TaskStarted {
        task_id: String,
        owner: String,
        purpose: String,
    },
    TaskProgress {
        task_id: String,
        message: String,
    },
    TaskCompleted {
        task_id: String,
        summary: String,
    },
    TaskFailed {
        task_id: String,
        error: String,
    },
    TaskCancelled {
        task_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PlanDecision {
    Approved,
    Revised { steps: Vec<String> },
    Rejected { reason: String },
}

/// Info about a single tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ToolCallInfo {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

// ===========================================================================
// MessageKind + SystemMessage
// ===========================================================================

/// What kind of message this is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageKind {
    Chat,
    System(SystemMessage),
    Stream(StreamEvent),
}

/// System-level messages for agent lifecycle and coordination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemMessage {
    Stop,
    JoinSession {
        session_id: SessionId,
        metadata: SessionMetadata,
    },
    LeaveSession {
        session_id: SessionId,
    },
    StatusUpdate {
        status: AgentStatus,
    },
    ApproveTool {
        call_id: String,
        approved: bool,
        scope: ApprovalScope,
        /// Optional user explanation when rejecting the request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    AnswerQuestion {
        call_id: String,
        answer: String,
    },
    /// Cancel only the active turn belonging to `session_id`.
    ///
    /// This is deliberately distinct from `Stop`, which terminates the whole
    /// Agent process and therefore affects every session it serves.
    InterruptTurn {
        session_id: SessionId,
    },
    ResolvePlan {
        plan_id: String,
        decision: PlanDecision,
    },
    CancelTask {
        session_id: SessionId,
        task_id: String,
    },
}

// ===========================================================================
// BusMessage
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Paste,
    File,
    Image,
    Selection,
    Diff,
    TerminalOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "encoding", rename_all = "snake_case")]
pub enum AttachmentContent {
    Text { text: String },
    Base64 { data: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MessageAttachment {
    pub id: String,
    pub kind: AttachmentKind,
    pub name: String,
    pub mime_type: String,
    pub content: AttachmentContent,
    pub byte_count: usize,
}

/// A message on the bus.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BusMessage {
    pub session_id: SessionId,
    pub sender: Sender,
    pub recipient: Recipient,
    pub kind: MessageKind,
    pub payload: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MessageAttachment>,
    pub timestamp: i64,
    pub id: MessageId,
}

/// Current Unix timestamp in seconds.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

impl BusMessage {
    pub fn user_chat(
        session_id: SessionId,
        user_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            session_id,
            sender: Sender::User(user_id.into()),
            recipient: Recipient::Broadcast,
            kind: MessageKind::Chat,
            payload: text.into(),
            attachments: Vec::new(),
            timestamp: now_secs(),
            id: MessageId::new(),
        }
    }

    pub fn user_chat_with_attachments(
        session_id: SessionId,
        user_id: impl Into<String>,
        text: impl Into<String>,
        attachments: Vec<MessageAttachment>,
    ) -> Self {
        let mut message = Self::user_chat(session_id, user_id, text);
        message.attachments = attachments;
        message
    }

    pub fn agent_response(
        session_id: SessionId,
        agent_id: AgentId,
        text: impl Into<String>,
    ) -> Self {
        Self {
            session_id,
            sender: Sender::Agent(agent_id),
            recipient: Recipient::Broadcast,
            kind: MessageKind::Chat,
            payload: text.into(),
            attachments: Vec::new(),
            timestamp: now_secs(),
            id: MessageId::new(),
        }
    }

    pub fn system_stop(agent_id: AgentId) -> Self {
        Self {
            session_id: SessionId::new(String::new()),
            sender: Sender::System,
            recipient: Recipient::Agent(agent_id),
            kind: MessageKind::System(SystemMessage::Stop),
            payload: String::new(),
            attachments: Vec::new(),
            timestamp: now_secs(),
            id: MessageId::new(),
        }
    }

    pub fn system_join_session(
        agent_id: AgentId,
        session_id: SessionId,
        metadata: SessionMetadata,
    ) -> Self {
        Self {
            session_id: session_id.clone(),
            sender: Sender::System,
            recipient: Recipient::Agent(agent_id),
            kind: MessageKind::System(SystemMessage::JoinSession {
                session_id,
                metadata,
            }),
            payload: String::new(),
            attachments: Vec::new(),
            timestamp: now_secs(),
            id: MessageId::new(),
        }
    }

    pub fn system_leave_session(agent_id: AgentId, session_id: SessionId) -> Self {
        Self {
            session_id: session_id.clone(),
            sender: Sender::System,
            recipient: Recipient::Agent(agent_id),
            kind: MessageKind::System(SystemMessage::LeaveSession { session_id }),
            payload: String::new(),
            attachments: Vec::new(),
            timestamp: now_secs(),
            id: MessageId::new(),
        }
    }

    pub fn system_interrupt_turn(agent_id: AgentId, session_id: SessionId) -> Self {
        Self {
            session_id: session_id.clone(),
            sender: Sender::System,
            recipient: Recipient::Agent(agent_id),
            kind: MessageKind::System(SystemMessage::InterruptTurn { session_id }),
            payload: String::new(),
            attachments: Vec::new(),
            timestamp: now_secs(),
            id: MessageId::new(),
        }
    }

    pub fn system_status_update(agent_id: AgentId, status: AgentStatus) -> Self {
        Self {
            session_id: SessionId::new(String::new()),
            sender: Sender::Agent(agent_id),
            recipient: Recipient::Broadcast,
            kind: MessageKind::System(SystemMessage::StatusUpdate { status }),
            payload: String::new(),
            attachments: Vec::new(),
            timestamp: now_secs(),
            id: MessageId::new(),
        }
    }

    pub fn stream_event(session_id: SessionId, agent_id: AgentId, event: StreamEvent) -> Self {
        Self {
            session_id,
            sender: Sender::Agent(agent_id),
            recipient: Recipient::Broadcast,
            kind: MessageKind::Stream(event),
            payload: String::new(),
            attachments: Vec::new(),
            timestamp: now_secs(),
            id: MessageId::new(),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/message.rs"]
mod tests;
