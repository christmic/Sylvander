//! Protocol-neutral data model used by the application and presentation.
//!
//! These types contain no terminal widgets, colors, socket clients, or input
//! events. They can be serialized, tested, or consumed by another renderer.

use std::path::PathBuf;

pub const CAPABILITY_VISION: u8 = 1 << 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMetadata {
    pub model: String,
    pub reasoning_effort: sylvander_protocol::ReasoningEffort,
    pub models: Vec<sylvander_protocol::ModelDescriptor>,
    pub permissions: sylvander_protocol::PermissionProfile,
    pub workspace: PathBuf,
    pub branch: String,
    pub capabilities: u8,
    pub approval_enabled: bool,
    pub max_attachment_bytes: usize,
}

impl Default for RuntimeMetadata {
    fn default() -> Self {
        Self {
            model: "—".into(),
            reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
            models: Vec::new(),
            permissions: sylvander_protocol::PermissionProfile::default(),
            workspace: PathBuf::from("~/workspace"),
            branch: "—".into(),
            capabilities: 0,
            approval_enabled: false,
            max_attachment_bytes: 512 * 1024,
        }
    }
}

impl RuntimeMetadata {
    pub fn supports_vision(&self) -> bool {
        self.capabilities & CAPABILITY_VISION != 0
    }

    pub fn model_label(&self) -> String {
        match self.reasoning_effort {
            sylvander_protocol::ReasoningEffort::Off => self.model.clone(),
            effort => format!("{} / {}", self.model, crate::app::reasoning_label(effort)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Normal,
    ApprovalPending,
    AskPending,
}

#[derive(Debug, Clone)]
pub enum ChatMessage {
    User(String),
    QueuedUser(String),
    Agent(String),
    ToolCall {
        name: String,
        status: ToolStatus,
        input: serde_json::Value,
    },
    ToolResult {
        name: String,
        output: String,
        ok: bool,
    },
    ToolStep {
        name: String,
        started_at_secs: u64,
        children: Vec<ToolStepChild>,
    },
    Thinking(String),
    Info(String),
    Plan {
        plan_id: String,
        steps: Vec<String>,
        current: usize,
    },
    TaskList {
        tasks: Vec<TaskEntry>,
    },
}

#[derive(Debug, Clone)]
pub struct ToolStepChild {
    pub call_id: String,
    pub name: String,
    pub status: ToolStatus,
    pub input: serde_json::Value,
    pub output: Option<String>,
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct TaskEntry {
    pub task_id: String,
    pub owner: String,
    pub purpose: String,
    pub state: TaskState,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Done,
    Error,
}

#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub label: String,
    pub workspace: String,
    pub last_seen_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryRole {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub role: HistoryRole,
    pub text: String,
}
