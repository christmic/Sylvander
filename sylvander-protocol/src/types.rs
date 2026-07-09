//! Wire-format data types — cross-language definitions.
//!
//! Every type here has `serde::Serialize/Deserialize` and
//! `schemars::JsonSchema` derives. The JSON Schema output is the
//! basis for TypeScript, Python, Swift, etc. code generation.

use std::path::PathBuf;

// use schemars::JsonSchema; // uncomment for multi-language schema gen
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ===========================================================================
// ID types
// ===========================================================================

/// Unique identifier for an agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for AgentId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}
impl From<String> for AgentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Unique identifier for a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for SessionId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Static metadata shared by all agents in a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub workspace: PathBuf,
    pub name: String,
    pub user_id: String,
}

// ===========================================================================
// Message envelope types
// ===========================================================================

/// Unique identifier for a bus message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sender {
    User(String),
    Agent(AgentId),
    System,
}

/// Who should receive the message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Recipient {
    Agent(AgentId),
    Broadcast,
}

/// Agent lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta { delta: String },
    ThinkingDelta { delta: String },
    ToolCall {
        call_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
    IterationStart { iteration: u32 },
    IterationEnd {
        iteration: u32,
        input_tokens: u32,
        output_tokens: u32,
    },
    Done { text: String },
    ToolApprovalRequired {
        batch_id: String,
        tools: Vec<ToolCallInfo>,
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
}

/// Info about a single tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallInfo {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

// ===========================================================================
// MessageKind + SystemMessage
// ===========================================================================

/// What kind of message this is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageKind {
    Chat,
    System(SystemMessage),
    Stream(StreamEvent),
}

/// System-level messages for agent lifecycle and coordination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    },
    AnswerQuestion {
        call_id: String,
        answer: String,
    },
}

// ===========================================================================
// BusMessage
// ===========================================================================

/// A message on the bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    pub session_id: SessionId,
    pub sender: Sender,
    pub recipient: Recipient,
    pub kind: MessageKind,
    pub payload: String,
    pub timestamp: i64,
    pub id: MessageId,
}

/// Current Unix timestamp in seconds.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
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
            timestamp: now_secs(),
            id: MessageId::new(),
        }
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
            timestamp: now_secs(),
            id: MessageId::new(),
        }
    }

    pub fn stream_event(
        session_id: SessionId,
        agent_id: AgentId,
        event: StreamEvent,
    ) -> Self {
        Self {
            session_id,
            sender: Sender::Agent(agent_id),
            recipient: Recipient::Broadcast,
            kind: MessageKind::Stream(event),
            payload: String::new(),
            timestamp: now_secs(),
            id: MessageId::new(),
        }
    }
}
