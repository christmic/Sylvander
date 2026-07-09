//! Domain events + Actions.
//!
//! `DomainEvent` is what flows INTO `AppState::apply()`. It is decoupled
//! from the wire protocol (`ServerMsg` lives in `client.rs`) and from
//! rendering concerns (panels/modals consume the resulting state).
//!
//! `Action` is what flows OUT — side effects the main loop must perform
//! (send a message, quit, etc.).

use serde_json::Value;

use crate::app::ToolInfo;

// ===========================================================================
// Inbound: DomainEvent
// ===========================================================================

/// A neutral, protocol-agnostic event. Anything that affects AppState
/// must be expressed as one of these.
#[derive(Debug, Clone)]
pub enum DomainEvent {
    /// Socket connected.
    Connected,
    /// Socket disconnected (graceful or otherwise).
    Disconnected { reason: String },

    /// Server assigned us a session id.
    SessionCreated { session_id: String },

    /// Streaming text chunk from the model.
    TextChunk { delta: String },
    /// Streaming thinking chunk from the model.
    ThinkingChunk { delta: String },
    /// A tool call started (status: pending).
    ToolStarted { tool_name: String, input: Value },
    /// A tool call finished.
    ToolFinished {
        tool_name: String,
        output: String,
        is_error: bool,
    },
    /// The agent loop has emitted its final answer.
    AgentDone { final_text: String },
    /// The agent loop failed.
    AgentError { message: String },

    /// Server wants permission to run one or more tools.
    ApprovalRequested {
        batch_id: String,
        tools: Vec<ToolInfo>,
    },

    /// Tick — heartbeat from the main loop (for spinner / time displays).
    Tick,
}

// ===========================================================================
// Outbound: Action
// ===========================================================================

/// A side effect the main loop should perform after applying an event.
#[derive(Debug, Clone)]
pub enum Action {
    /// Send a chat message to the server.
    SendChat {
        text: String,
        session_id: Option<String>,
    },
    /// Approve or reject a specific tool call.
    SendApprove { call_id: String, approved: bool },
    /// Answer an AskUser question.
    SendAnswer { call_id: String, answer: String },
    /// User wants to quit.
    Quit,
}