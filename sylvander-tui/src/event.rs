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
use crate::model::{HistoryEntry, SessionSummary};

// ===========================================================================
// Inbound: DomainEvent
// ===========================================================================

/// A neutral, protocol-agnostic event. Anything that affects AppState
/// must be expressed as one of these.
#[derive(Debug, Clone)]
pub enum DomainEvent {
    /// Socket connected.
    Connected,
    ProtocolNegotiated {
        version: u16,
        server_name: String,
        capabilities: Vec<String>,
    },
    ProtocolDiagnostic {
        message: String,
    },
    RuntimeInfo {
        model: String,
        reasoning_effort: sylvander_protocol::ReasoningEffort,
        models: Vec<sylvander_protocol::ModelDescriptor>,
        permissions: sylvander_protocol::PermissionProfile,
        capabilities: u8,
        approval_enabled: bool,
        max_attachment_bytes: usize,
        platform: sylvander_protocol::PlatformSnapshot,
    },
    ContextReported {
        report: sylvander_protocol::ContextReport,
    },
    CompactionStarted {
        automatic: bool,
    },
    CompactionCompleted {
        report: sylvander_protocol::CompactionReport,
    },
    CompactionFailed {
        automatic: bool,
        reason: String,
    },
    WorkspaceRollbackPreviewed {
        session_id: String,
        preview: sylvander_protocol::WorkspaceRollbackPreview,
    },
    WorkspaceRollbackCompleted {
        report: sylvander_protocol::WorkspaceRollbackReport,
    },
    WorkspaceRollbackFailed {
        reason: String,
    },
    WorkspaceDiffLoaded {
        scope: WorkspaceDiffScope,
        diff: String,
    },
    WorkspaceDiffFailed {
        reason: String,
    },
    WorkspaceReviewLoaded {
        scope: WorkspaceDiffScope,
        diff: String,
    },
    WorkspaceReviewFailed {
        reason: String,
    },
    ConfigInspected {
        report: String,
    },
    DoctorCompleted {
        report: Option<String>,
        message: String,
    },
    DoctorFailed {
        reason: String,
    },
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
    SessionHistoryLoaded {
        session: SessionSummary,
        messages: Vec<HistoryEntry>,
        iterations: u32,
        input_tokens: u64,
        output_tokens: u64,
        cost_nano_usd: Option<u64>,
        notice: Option<String>,
        source_session_id: Option<String>,
        recovery: bool,
        replay_truncated: bool,
    },
    SessionUpdated {
        session_id: String,
        label: Option<String>,
        archived: bool,
    },
    SessionDeleted {
        session_id: String,
    },
    OperationFailed {
        operation: String,
        message: String,
    },

    /// Streaming text chunk from the model.
    TextChunk {
        delta: String,
    },
    /// Streaming thinking chunk from the model.
    ThinkingChunk {
        delta: String,
    },
    ModelRetry {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        reason: String,
        cause: sylvander_protocol::RetryCause,
    },
    InteractionTimedOut {
        kind: sylvander_protocol::InteractionTimeoutKind,
        subject_id: String,
        timeout_secs: u64,
        recovery: sylvander_protocol::TimeoutRecovery,
    },
    /// A tool call started (status: pending).
    ToolStarted {
        call_id: String,
        tool_name: String,
        input: Value,
    },
    ToolOutputDelta {
        call_id: String,
        tool_name: String,
        delta: String,
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
        input_tokens: u64,
        output_tokens: u64,
        cost_nano_usd: Option<u64>,
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
        allowed_scopes: Vec<sylvander_protocol::ApprovalScope>,
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
    PlanUpdated {
        plan_id: String,
        steps: Vec<String>,
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
        attachments: Vec<sylvander_protocol::MessageAttachment>,
        session_id: Option<String>,
        workspace: String,
    },
    /// Approve or reject a specific tool call.
    SendApprove {
        call_id: String,
        approved: bool,
        scope: sylvander_protocol::ApprovalScope,
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
    ResolvePlan {
        plan_id: String,
        decision: sylvander_protocol::PlanDecision,
    },
    CancelTask {
        session_id: String,
        task_id: String,
    },
    RequestSessions,
    RequestRuntimeInfo,
    RequestContext {
        session_id: Option<String>,
    },
    CompactSession {
        session_id: String,
    },
    PreviewWorkspaceRollback {
        session_id: String,
    },
    ConfirmWorkspaceRollback {
        session_id: String,
        expected_turn_id: String,
    },
    SelectModel {
        model: String,
        reasoning_effort: sylvander_protocol::ReasoningEffort,
    },
    SelectPermissions {
        profile: sylvander_protocol::PermissionProfile,
    },
    LoadSession {
        session_id: String,
    },
    ReconcileSession {
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
    CopyText {
        text: String,
    },
    EditDraft,
    InspectWorkspaceDiff {
        scope: WorkspaceDiffScope,
        workspace: std::path::PathBuf,
    },
    ReviewWorkspaceChanges {
        scope: WorkspaceDiffScope,
        workspace: std::path::PathBuf,
    },
    InspectConfig,
    RunDoctor {
        destination: DoctorDestination,
    },
    ForkSession {
        session_id: String,
        completed_turns: Option<usize>,
        checkpoint: bool,
    },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceDiffScope {
    All,
    Staged,
    Unstaged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorDestination {
    Inspect,
    Copy,
    Export(std::path::PathBuf),
}

impl WorkspaceDiffScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all changes",
            Self::Staged => "staged changes",
            Self::Unstaged => "unstaged changes",
        }
    }
}
