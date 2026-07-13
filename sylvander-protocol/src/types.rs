//! Wire-format data types — cross-language definitions.
//!
//! Every type here has `serde::Serialize/Deserialize` and
//! `schemars::JsonSchema` derives. The JSON Schema output is the
//! basis for TypeScript, Python, Swift, etc. code generation.

use std::path::PathBuf;

// use schemars::JsonSchema; // uncomment for multi-language schema gen
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const UI_PROTOCOL_MIN_VERSION: u16 = 1;
pub const UI_PROTOCOL_MAX_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiProtocolHello {
    pub client_name: String,
    pub min_version: u16,
    pub max_version: u16,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiProtocolWelcome {
    pub server_name: String,
    pub version: u16,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiProtocolError {
    pub code: String,
    pub message: String,
    pub server_min_version: u16,
    pub server_max_version: u16,
}

pub fn negotiate_ui_protocol(hello: &UiProtocolHello) -> Result<u16, UiProtocolError> {
    let selected = hello.max_version.min(UI_PROTOCOL_MAX_VERSION);
    let required_min = hello.min_version.max(UI_PROTOCOL_MIN_VERSION);
    if hello.min_version <= hello.max_version && selected >= required_min {
        return Ok(selected);
    }
    Err(UiProtocolError {
        code: "incompatible_protocol".into(),
        message: format!(
            "client supports {}..={}, server supports {}..={}",
            hello.min_version, hello.max_version, UI_PROTOCOL_MIN_VERSION, UI_PROTOCOL_MAX_VERSION
        ),
        server_min_version: UI_PROTOCOL_MIN_VERSION,
        server_max_version: UI_PROTOCOL_MAX_VERSION,
    })
}

/// User-facing reasoning intensity. The runtime maps these stable semantic
/// levels to provider-specific token budgets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    #[must_use]
    pub fn budget_tokens(self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::Low => Some(2_048),
            Self::Medium => Some(8_192),
            Self::High => Some(20_000),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub provider: String,
    pub capabilities: u8,
    pub reasoning_efforts: Vec<ReasoningEffort>,
    #[serde(default)]
    pub lifecycle: ModelLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
}

/// Operator-supplied API prices in micro-US-dollars per million tokens.
/// `1_000_000` therefore means `$1.00 / 1M tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_usd_micros_per_million: u64,
    pub output_usd_micros_per_million: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_usd_micros_per_million: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_usd_micros_per_million: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ModelLifecycle {
    #[default]
    Active,
    Deprecated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replacement: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeModelInfo {
    pub current_model: String,
    pub reasoning_effort: ReasoningEffort,
    pub models: Vec<ModelDescriptor>,
}

/// UI-oriented classification for optional Agent platform facilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformFeatureKind {
    Mcp,
    Skill,
    Memory,
    Hook,
    Extension,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformFeatureStatus {
    Active,
    Configured,
    Degraded,
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformAuthStatus {
    NotRequired,
    Configured,
    Missing,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformTrust {
    BuiltIn,
    Workspace,
    User,
    External,
    Unverified,
}

/// Redacted platform truth intended for status and inspection surfaces. It
/// deliberately excludes credentials, command arguments, and filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformFeature {
    pub kind: PlatformFeatureKind,
    pub name: String,
    #[serde(default)]
    pub status: PlatformFeatureStatus,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<PlatformTrust>,
    #[serde(default)]
    pub auth: PlatformAuthStatus,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub reloadable: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformSnapshot {
    #[serde(default)]
    pub features: Vec<PlatformFeature>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSourceKind {
    SystemPrompt,
    Conversation,
    Tools,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSource {
    pub kind: ContextSourceKind,
    pub label: String,
    pub items: usize,
}

/// Last provider-confirmed context usage plus its structural contributors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextReport {
    pub model: String,
    pub context_window: u32,
    pub used_tokens: u32,
    pub remaining_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub sources: Vec<ContextSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionReport {
    pub automatic: bool,
    pub removed_messages: usize,
    pub condensed_blocks: usize,
    pub freed_tokens: u32,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRollbackPreview {
    pub turn_id: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRollbackReport {
    pub turn_id: String,
    pub restored: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryCause {
    RateLimit,
    Server,
    Network,
    Stream,
    #[default]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionTimeoutKind {
    Approval,
    Question,
    Plan,
    Tool,
    Task,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutRecovery {
    RetryRequest,
    NarrowScope,
    ContinueWithout,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAccess {
    None,
    ReadOnly,
    #[default]
    WorkspaceWrite,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAccess {
    #[default]
    Denied,
    Allowed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    Ask,
    #[default]
    Allow,
    Deny,
}

/// Lifetime requested for an approved tool capability.
///
/// Transports must forward this value unchanged. The Agent remains the
/// authority that decides which scopes are allowed for a request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    #[default]
    Once,
    Session,
    Persistent,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionProfile {
    pub file_access: FileAccess,
    pub network_access: NetworkAccess,
    pub approval_policy: ApprovalPolicy,
}

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

/// Unique identifier for a human user.
///
/// Distinct from `AgentId` (the LLM-driven runtime) and `SessionId`
/// (a single conversation). One user may own many agents and run many
/// sessions; one session is bound to exactly one user; one agent is
/// owned by exactly one user.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);

impl UserId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Sentinel for system-originated actions (cron, internal tasks)
    /// that have no real user. Distinct from any real `UserId`.
    pub fn system() -> Self {
        Self("__system__".to_string())
    }
}

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for UserId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for UserId {
    fn from(s: String) -> Self {
        Self(s)
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
        #[serde(default)]
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
    ToolApprovalRequired {
        batch_id: String,
        tools: Vec<ToolCallInfo>,
        /// Scopes the operator permits for this request. `Once` is always
        /// present; persistent approval is never implied by the UI.
        #[serde(default = "default_approval_scopes")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PlanDecision {
    Approved,
    Revised { steps: Vec<String> },
    Rejected { reason: String },
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
        #[serde(default)]
        scope: ApprovalScope,
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

fn default_approval_scopes() -> Vec<ApprovalScope> {
    vec![ApprovalScope::Once]
}

// ===========================================================================
// BusMessage
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Paste,
    File,
    Image,
    Selection,
    Diff,
    TerminalOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case")]
pub enum AttachmentContent {
    Text { text: String },
    Base64 { data: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAttachment {
    pub id: String,
    pub kind: AttachmentKind,
    pub name: String,
    pub mime_type: String,
    pub content: AttachmentContent,
    pub byte_count: usize,
}

/// A message on the bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
mod tests {
    use super::*;

    #[test]
    fn user_id_round_trips() {
        let u: UserId = "alice".into();
        assert_eq!(u.0, "alice");
        let u2: UserId = String::from("bob").into();
        assert_eq!(u2.0, "bob");
        assert_eq!(u.to_string(), "alice");
    }

    #[test]
    fn user_id_system_sentinel_is_distinct() {
        let sys = UserId::system();
        let real = UserId::new("alice");
        assert_ne!(sys, real);
        assert_ne!(sys.0, "alice");
    }

    #[test]
    fn user_id_serializes_as_inner_string() {
        let u = UserId::new("alice");
        let json = serde_json::to_string(&u).unwrap();
        assert_eq!(json, "\"alice\"");
    }

    #[test]
    fn three_id_types_share_a_constructor_pattern() {
        // Smoke: AgentId / SessionId / UserId all have the same shape.
        let _a: AgentId = "a".into();
        let _s: SessionId = "s".into();
        let _u: UserId = "u".into();
    }

    #[test]
    fn legacy_bus_messages_default_to_no_attachments() {
        let mut value =
            serde_json::to_value(BusMessage::user_chat("s".into(), "u", "hi")).expect("serialize");
        value.as_object_mut().unwrap().remove("attachments");
        let message: BusMessage = serde_json::from_value(value).expect("legacy decode");
        assert!(message.attachments.is_empty());
    }

    #[test]
    fn reasoning_effort_has_stable_provider_neutral_budgets() {
        assert_eq!(ReasoningEffort::Off.budget_tokens(), None);
        assert_eq!(ReasoningEffort::Low.budget_tokens(), Some(2_048));
        assert_eq!(ReasoningEffort::Medium.budget_tokens(), Some(8_192));
        assert_eq!(ReasoningEffort::High.budget_tokens(), Some(20_000));
    }

    #[test]
    fn legacy_approval_messages_default_to_one_shot_scope() {
        let system: SystemMessage = serde_json::from_value(serde_json::json!({
            "type": "approve_tool",
            "call_id": "call-1",
            "approved": true
        }))
        .expect("legacy system message");
        assert!(matches!(
            system,
            SystemMessage::ApproveTool {
                scope: ApprovalScope::Once,
                ..
            }
        ));

        let event: StreamEvent = serde_json::from_value(serde_json::json!({
            "type": "tool_approval_required",
            "batch_id": "batch-1",
            "tools": []
        }))
        .expect("legacy stream event");
        assert!(matches!(
            event,
            StreamEvent::ToolApprovalRequired { allowed_scopes, .. }
                if allowed_scopes == vec![ApprovalScope::Once]
        ));
    }

    #[test]
    fn legacy_retry_events_default_to_other_cause() {
        let event: StreamEvent = serde_json::from_value(serde_json::json!({
            "type": "model_retry",
            "attempt": 1,
            "max_attempts": 3,
            "delay_ms": 100,
            "reason": "temporary"
        }))
        .expect("legacy retry event");
        assert!(matches!(
            event,
            StreamEvent::ModelRetry {
                cause: RetryCause::Other,
                ..
            }
        ));
    }

    #[test]
    fn legacy_model_descriptors_default_to_active() {
        let descriptor: ModelDescriptor = serde_json::from_value(serde_json::json!({
            "id": "model-a",
            "provider": "test",
            "capabilities": 0,
            "reasoning_efforts": ["off"]
        }))
        .expect("legacy model descriptor");
        assert_eq!(descriptor.lifecycle, ModelLifecycle::Active);
        assert_eq!(descriptor.pricing, None);
    }

    #[test]
    fn platform_snapshot_round_trip_keeps_status_semantic() {
        let snapshot = PlatformSnapshot {
            features: vec![PlatformFeature {
                kind: PlatformFeatureKind::Mcp,
                name: "code search".into(),
                status: PlatformFeatureStatus::Configured,
                summary: "configured".into(),
                source: Some("search-mcp".into()),
                trust: Some(PlatformTrust::External),
                auth: PlatformAuthStatus::Configured,
                capabilities: vec!["tools".into()],
                reloadable: false,
            }],
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let restored: PlatformSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, snapshot);
    }

    #[test]
    fn ui_protocol_selects_overlap_and_rejects_incompatible_ranges() {
        let compatible = UiProtocolHello {
            client_name: "test".into(),
            min_version: 1,
            max_version: 2,
            capabilities: vec!["diagnostics".into()],
        };
        assert_eq!(negotiate_ui_protocol(&compatible), Ok(1));

        let incompatible = UiProtocolHello {
            min_version: 2,
            max_version: 3,
            ..compatible
        };
        let error = negotiate_ui_protocol(&incompatible).expect_err("must reject");
        assert_eq!(error.code, "incompatible_protocol");
        assert_eq!(error.server_max_version, UI_PROTOCOL_MAX_VERSION);
    }
}
