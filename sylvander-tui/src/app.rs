//! Application state and message types.

use crate::input::InputState;

/// Top-level application state.
pub struct AppState {
    pub should_quit: bool,
    pub input: InputState,
    pub messages: Vec<ChatMessage>,
    pub streaming: String,
    pub session_id: Option<String>,
    pub mode: AppMode,
    pub connected: bool,
    pub status: String,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            should_quit: false,
            input: InputState::default(),
            messages: Vec::new(),
            streaming: String::new(),
            session_id: None,
            mode: AppMode::Normal,
            connected: false,
            status: "Connecting...".into(),
        }
    }
}

/// Application mode — controls what the input field does.
pub enum AppMode {
    Normal,
    Approval {
        batch_id: String,
        tools: Vec<ToolInfo>,
        current: usize,
        decisions: Vec<bool>,
    },
    AskUser {
        call_id: String,
        question: String,
        options: Vec<String>,
        answer: String,
    },
}

/// Tool info for approval.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

/// A message in the chat history.
#[derive(Debug, Clone)]
pub enum ChatMessage {
    User(String),
    Agent(String),
    ToolCall { name: String, status: ToolStatus },
    ToolResult { name: String, output: String, ok: bool },
    Thinking(String),
    Info(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    Pending,
    Done,
    Error,
}
