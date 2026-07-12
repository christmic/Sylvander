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
use crate::model::SessionSummary;

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
    Disconnected {
        reason: String,
    },

    /// Server assigned us a session id.
    SessionCreated {
        session_id: String,
    },
    SessionsLoaded {
        sessions: Vec<SessionSummary>,
    },

    /// Streaming text chunk from the model.
    TextChunk {
        delta: String,
    },
    /// Streaming thinking chunk from the model.
    ThinkingChunk {
        delta: String,
    },
    /// A tool call started (status: pending).
    ToolStarted {
        call_id: String,
        tool_name: String,
        input: Value,
    },
    /// A tool call finished.
    ToolFinished {
        call_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
    UsageUpdated {
        iteration: u32,
        input_tokens: u32,
        output_tokens: u32,
    },
    /// The agent loop has emitted its final answer.
    AgentDone {
        final_text: String,
    },
    /// The agent loop failed.
    AgentError {
        message: String,
    },
    /// The server confirmed that the active turn ended by user interrupt.
    TurnInterrupted {
        reason: String,
    },

    /// Server wants permission to run one or more tools.
    ApprovalRequested {
        batch_id: String,
        tools: Vec<ToolInfo>,
    },

    /// Agent asks the user a clarifying question (UX §12.1).
    /// `options.len() == 0` → free-text only.
    /// `options.len() > 0 && multi_select == false` → single-select with
    /// free-text fallback.
    /// `options.len() > 0 && multi_select == true` → multi-select with
    /// free-text fallback.
    AskUserRequested {
        call_id: String,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    },

    /// Server rejected a tool call (its policy disallows it). Surface
    /// the reason so the user can adjust or report.
    ToolRejected {
        tool_name: String,
        reason: String,
    },

    /// Agent is presenting a plan before doing more work (UX §9). The
    /// TUI renders the plan inline in the transcript and pushes a
    /// `PlanReviewModal` so the user can approve / edit / cancel.
    PlanReceived {
        plan_id: String,
        steps: Vec<String>,
        /// Currently-active step index (the ◉ in the marker row).
        current: usize,
    },

    /// Agent kicked off a background task / subagent (UX §11). Surfaces
    /// as a `TaskList` line in the transcript and tracks in-flight vs
    /// completed count via repeated `TaskProgress` events.
    TaskStarted {
        task_id: String,
        owner: String,
        purpose: String,
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
    SendApprove {
        call_id: String,
        approved: bool,
    },
    /// Answer an AskUser question.
    SendAnswer {
        call_id: String,
        answer: String,
    },
    /// Interrupt the active turn for one session without stopping the Agent.
    InterruptTurn {
        session_id: String,
    },
    RequestSessions,
    /// Send a feedback message to the agent (e.g. after rejecting a tool
    /// call, so it can adjust its next attempt). Wraps as a chat message
    /// with a `[/feedback]` prefix the agent loop recognizes; the wire
    /// stays a plain `ClientMsg::Chat`.
    SendFeedback {
        text: String,
        session_id: Option<String>,
    },
    /// User wants to quit.
    Quit,
}
