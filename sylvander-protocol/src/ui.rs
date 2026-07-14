//! Public, UI-facing service protocol.
//!
//! Transports encode these messages; they do not define competing wire types.

use serde::{Deserialize, Serialize};

use crate::{
    ApprovalScope, MessageAttachment, PermissionProfile, PlanDecision, ReasoningEffort,
    UiProtocolHello,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiClientMessage {
    Hello {
        protocol: UiProtocolHello,
    },
    Chat {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<MessageAttachment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    Approve {
        #[serde(default)]
        session_id: String,
        call_id: String,
        approved: bool,
        #[serde(default)]
        scope: ApprovalScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Answer {
        #[serde(default)]
        session_id: String,
        call_id: String,
        answer: String,
    },
    Interrupt {
        session_id: String,
    },
    ResolvePlan {
        #[serde(default)]
        session_id: String,
        plan_id: String,
        decision: PlanDecision,
    },
    CancelTask {
        session_id: String,
        task_id: String,
    },
    DiscoverAgents,
    GetSessionConfig {
        session_id: String,
    },
    UpdateSessionConfig {
        request: crate::SessionConfigUpdateRequest,
    },
    SubmitFeedback {
        feedback: crate::RunFeedback,
    },
    ListSessions,
    LoadSession {
        session_id: String,
    },
    ReattachSession {
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completed_turns: Option<usize>,
        #[serde(default)]
        checkpoint: bool,
    },
    GetRuntimeInfo,
    GetContext {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    Compact {
        session_id: String,
    },
    PreviewWorkspaceRollback {
        session_id: String,
    },
    RollbackWorkspace {
        session_id: String,
        expected_turn_id: String,
    },
    SelectModel {
        model: String,
        reasoning_effort: ReasoningEffort,
    },
    SelectPermissions {
        profile: PermissionProfile,
    },
    Ping,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiServerMessage {
    Welcome {
        protocol: crate::UiProtocolWelcome,
    },
    ProtocolError {
        error: crate::UiProtocolError,
    },
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
        #[serde(default)]
        cause: crate::RetryCause,
    },
    InteractionTimeout {
        session_id: String,
        kind: crate::InteractionTimeoutKind,
        subject_id: String,
        timeout_secs: u64,
        recovery: crate::TimeoutRecovery,
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
        #[serde(default)]
        cost_nano_usd: Option<u64>,
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
        tools: Vec<UiToolInfo>,
        #[serde(default = "default_approval_scopes")]
        allowed_scopes: Vec<ApprovalScope>,
    },
    ToolRejected {
        session_id: String,
        tool_name: String,
        reason: String,
    },
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
        sessions: Vec<UiSessionInfo>,
    },
    SessionHistory {
        session: UiSessionInfo,
        messages: Vec<UiHistoryMessage>,
        iterations: u32,
        input_tokens: u64,
        output_tokens: u64,
        #[serde(default)]
        cost_nano_usd: Option<u64>,
        #[serde(default)]
        notice: Option<String>,
        #[serde(default)]
        source_session_id: Option<String>,
        #[serde(default)]
        recovery: bool,
        #[serde(default)]
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
    AgentsDiscovered {
        agents: Vec<crate::AgentDescriptor>,
    },
    SessionConfig {
        state: crate::SessionConfigState,
    },
    FeedbackRecorded {
        feedback_id: String,
    },
    RuntimeInfo {
        model: String,
        #[serde(default)]
        reasoning_effort: ReasoningEffort,
        #[serde(default)]
        models: Vec<crate::ModelDescriptor>,
        #[serde(default)]
        permissions: PermissionProfile,
        capabilities: u8,
        approval_enabled: bool,
        max_attachment_bytes: usize,
        #[serde(default)]
        platform: crate::PlatformSnapshot,
    },
    ContextReport {
        report: crate::ContextReport,
    },
    CompactionStarted {
        session_id: String,
        automatic: bool,
    },
    CompactionCompleted {
        session_id: String,
        report: crate::CompactionReport,
    },
    CompactionFailed {
        session_id: String,
        automatic: bool,
        reason: String,
    },
    WorkspaceRollbackPreview {
        session_id: String,
        preview: crate::WorkspaceRollbackPreview,
    },
    WorkspaceRollbackCompleted {
        session_id: String,
        report: crate::WorkspaceRollbackReport,
    },
    WorkspaceRollbackFailed {
        session_id: String,
        reason: String,
    },
    OperationError {
        operation: String,
        message: String,
    },
    Pong,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiToolInfo {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiSessionInfo {
    pub id: String,
    pub label: String,
    pub workspace: String,
    pub last_seen_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiHistoryMessage {
    pub role: String,
    pub text: String,
}

fn default_approval_scopes() -> Vec<ApprovalScope> {
    vec![ApprovalScope::Once]
}

#[cfg(test)]
mod tests {
    use super::UiClientMessage;

    #[test]
    fn legacy_chat_defaults_remain_compatible() {
        let message: UiClientMessage =
            serde_json::from_str(r#"{"type":"chat","text":"hello"}"#).unwrap();
        assert!(matches!(
            message,
            UiClientMessage::Chat {
                attachments,
                session_id: None,
                workspace: None,
                ..
            } if attachments.is_empty()
        ));
    }
}
