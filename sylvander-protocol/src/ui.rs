//! Public, UI-facing service protocol.
//!
//! Transports encode these messages; they do not define competing wire types.

use serde::{Deserialize, Serialize};

use crate::{
    ApprovalScope, MessageAttachment, PermissionProfile, PlanDecision, ReasoningEffort,
    UiProtocolHello,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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
    CreateSession {
        request: crate::SessionCreateRequest,
    },
    GetSessionConfig {
        session_id: String,
    },
    UpdateSessionConfig {
        request: crate::SessionConfigUpdateRequest,
    },
    SubmitFeedback {
        feedback: crate::RunFeedback,
    },
    AgentAdmin {
        request: crate::AgentAdminRequest,
    },
    RegistryAdmin {
        request: crate::RegistryAdminRequest,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        model: crate::ModelSelectionInput,
        reasoning_effort: ReasoningEffort,
    },
    SelectPermissions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        profile: PermissionProfile,
    },
    Ping,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<crate::SessionConfigState>,
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
    AgentAdmin {
        response: crate::AgentAdminResponse,
    },
    RegistryAdmin {
        response: crate::RegistryAdminResponse,
    },
    RuntimeInfo {
        /// Legacy model-only identity retained for older clients.
        model: String,
        /// Provider-qualified identity used by current clients.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_selection: Option<crate::ModelSelection>,
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
    BoundaryDenied {
        error: crate::BoundaryError,
    },
    Pong,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UiToolInfo {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UiSessionInfo {
    pub id: String,
    pub label: String,
    pub workspace: String,
    pub last_seen_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UiHistoryMessage {
    pub role: String,
    pub text: String,
}

fn default_approval_scopes() -> Vec<ApprovalScope> {
    vec![ApprovalScope::Once]
}

#[cfg(test)]
mod tests {
    use super::{UiClientMessage, UiServerMessage};
    use crate::{PermissionProfile, ReasoningEffort};

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

    #[test]
    fn model_selection_accepts_legacy_and_qualified_wire_shapes() {
        let legacy: UiClientMessage =
            serde_json::from_str(r#"{"type":"select_model","model":"m","reasoning_effort":"off"}"#)
                .unwrap();
        assert!(matches!(
            legacy,
            UiClientMessage::SelectModel {
                session_id: None,
                model: crate::ModelSelectionInput::Legacy(model),
                ..
            } if model == "m"
        ));

        let qualified = UiClientMessage::SelectModel {
            session_id: Some("session-1".into()),
            model: crate::ModelSelectionInput::Qualified(crate::ModelSelection {
                provider_id: "openai".into(),
                model_id: "gpt-5".into(),
            }),
            reasoning_effort: ReasoningEffort::High,
        };
        let value = serde_json::to_value(&qualified).unwrap();
        assert_eq!(value["session_id"], "session-1");
        assert_eq!(value["model"]["provider_id"], "openai");
        assert_eq!(value["model"]["model_id"], "gpt-5");
        assert_eq!(
            serde_json::from_value::<UiClientMessage>(value).unwrap(),
            qualified
        );
    }

    #[test]
    fn runtime_info_adds_qualified_identity_without_breaking_legacy_payloads() {
        let legacy: UiServerMessage = serde_json::from_value(serde_json::json!({
            "type": "runtime_info",
            "model": "shared",
            "capabilities": 0,
            "approval_enabled": false,
            "max_attachment_bytes": 1024
        }))
        .unwrap();
        assert!(matches!(
            legacy,
            UiServerMessage::RuntimeInfo {
                model_selection: None,
                ..
            }
        ));

        let qualified = UiServerMessage::RuntimeInfo {
            model: "shared".into(),
            model_selection: Some(crate::ModelSelection {
                provider_id: "openai".into(),
                model_id: "shared".into(),
            }),
            reasoning_effort: ReasoningEffort::Off,
            models: Vec::new(),
            permissions: PermissionProfile::default(),
            capabilities: 0,
            approval_enabled: false,
            max_attachment_bytes: 1024,
            platform: crate::PlatformSnapshot::default(),
        };
        let value = serde_json::to_value(qualified).unwrap();
        assert_eq!(value["model"], "shared");
        assert_eq!(value["model_selection"]["provider_id"], "openai");
        assert_eq!(value["model_selection"]["model_id"], "shared");
    }

    #[test]
    fn model_selection_schema_exposes_legacy_and_qualified_inputs() {
        let schema = serde_json::to_string(&crate::schema::ui_protocol_schema()).unwrap();
        assert!(schema.contains("ModelSelectionInput"));
        assert!(schema.contains("ModelSelection"));
        assert!(schema.contains("model_selection"));
    }

    #[test]
    fn selection_permissions_wire_keeps_session_identity() {
        let value = serde_json::to_value(UiClientMessage::SelectPermissions {
            session_id: Some("session-1".into()),
            profile: crate::PermissionProfile::default(),
        })
        .unwrap();
        assert_eq!(value["session_id"], "session-1");
    }

    #[test]
    fn agent_administration_uses_one_transport_envelope() {
        let client: UiClientMessage = serde_json::from_value(serde_json::json!({
            "type": "agent_admin",
            "request": {
                "operation": "activate_revision",
                "agent_id": "oraculo",
                "revision": 5,
                "expected_active_revision": 4
            }
        }))
        .unwrap();
        assert!(matches!(
            client,
            UiClientMessage::AgentAdmin {
                request: crate::AgentAdminRequest::ActivateRevision {
                    revision: 5,
                    expected_active_revision: 4,
                    ..
                }
            }
        ));

        let server = UiServerMessage::AgentAdmin {
            response: crate::AgentAdminResponse::Error {
                error: crate::AgentAdminError {
                    code: crate::AgentAdminErrorCode::RevisionConflict,
                    message: "active revision changed".into(),
                    agent_id: Some(crate::AgentId::new("oraculo")),
                    revision: Some(5),
                    expected_active_revision: Some(4),
                    actual_active_revision: Some(6),
                },
            },
        };
        let json = serde_json::to_value(server).unwrap();
        assert_eq!(json["type"], "agent_admin");
        assert_eq!(json["response"]["error"]["code"], "revision_conflict");
    }

    #[test]
    fn registry_administration_uses_one_transport_envelope() {
        let client = UiClientMessage::RegistryAdmin {
            request: crate::RegistryAdminRequest::InspectProviderRevision {
                provider_id: "alpha".into(),
                revision: 2,
            },
        };
        let client_json = serde_json::to_value(&client).unwrap();
        assert_eq!(client_json["type"], "registry_admin");
        assert_eq!(
            serde_json::from_value::<UiClientMessage>(client_json).unwrap(),
            client
        );

        let server = UiServerMessage::RegistryAdmin {
            response: crate::RegistryAdminResponse::Error {
                error: crate::RegistryAdminError {
                    code: crate::RegistryAdminErrorCode::StorageUnavailable,
                    message: "registry unavailable".into(),
                    provider_id: None,
                    revision: None,
                },
            },
        };
        let server_json = serde_json::to_value(&server).unwrap();
        assert_eq!(server_json["type"], "registry_admin");
        assert_eq!(
            serde_json::from_value::<UiServerMessage>(server_json).unwrap(),
            server
        );
    }
}
