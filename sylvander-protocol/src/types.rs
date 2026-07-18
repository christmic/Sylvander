//! Wire-format data types — cross-language definitions.
//!
//! Every type here has `serde::Serialize/Deserialize` and
//! `schemars::JsonSchema` derives. The JSON Schema output is the
//! basis for TypeScript, Python, Swift, etc. code generation.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The only UI protocol revision accepted by this pre-release build.
///
/// Sylvander intentionally ships one latest schema before its first stable
/// release. Older or newer revisions fail negotiation instead of entering a
/// compatibility path.
pub const UI_PROTOCOL_VERSION: u16 = 5;
pub const UI_PROTOCOL_MIN_VERSION: u16 = UI_PROTOCOL_VERSION;
pub const UI_PROTOCOL_MAX_VERSION: u16 = UI_PROTOCOL_VERSION;
/// Negotiated UI capability for opaque, evidence-backed turn feedback.
pub const FEEDBACK_CAPABILITY: &str = "feedback_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UiProtocolHello {
    pub client_name: String,
    pub min_version: u16,
    pub max_version: u16,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UiProtocolWelcome {
    pub server_name: String,
    pub version: u16,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelDescriptor {
    pub id: String,
    pub provider: String,
    /// Compact capability bitset used by terminal clients.
    pub capabilities: u8,
    /// Provider-neutral, canonical capabilities for current clients.
    pub capability_names: Vec<ModelCapability>,
    pub reasoning_efforts: Vec<ReasoningEffort>,
    pub lifecycle: ModelLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
}

/// Canonical model capabilities exposed by the public protocol.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    ExtendedThinking,
    PromptCaching,
    StructuredOutput,
    ToolUse,
    Vision,
    DocumentInput,
}

/// Stable identity for one model exposed by one provider.
///
/// Model ids are not globally unique. Persisted selections and new wire
/// requests therefore use both fields as one indivisible identity.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelSelectionResolutionError {
    #[error("model selection `{provider_id}/{model_id}` is unavailable")]
    Unavailable {
        provider_id: String,
        model_id: String,
    },
}

/// Operator-supplied API prices in micro-US-dollars per million tokens.
/// `1_000_000` therefore means `$1.00 / 1M tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelPricing {
    pub input_usd_micros_per_million: u64,
    pub output_usd_micros_per_million: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_usd_micros_per_million: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_usd_micros_per_million: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ModelLifecycle {
    #[default]
    Active,
    Deprecated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replacement: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RuntimeModelInfo {
    pub current_model: String,
    pub reasoning_effort: ReasoningEffort,
    pub models: Vec<ModelDescriptor>,
}

/// UI-oriented classification for optional Agent platform facilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlatformFeatureKind {
    Mcp,
    Skill,
    Memory,
    Hook,
    Extension,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PlatformFeatureStatus {
    Active,
    Configured,
    Degraded,
    #[default]
    Unavailable,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PlatformAuthStatus {
    NotRequired,
    Configured,
    Missing,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

/// A transport-neutral effect contributed by an optional platform facility.
/// The TUI remains responsible for applying the effect through its normal
/// application boundary; extensions never receive presentation callbacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiCommandEffect {
    /// Expand a trusted template and submit it through the ordinary chat path.
    /// `{{args}}` is replaced with the user-supplied command arguments.
    SubmitPrompt { template: String },
}

/// Redacted command metadata advertised to UI clients. Names and trust are
/// validated again by the client because built-in command sets can differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UiCommandDescriptor {
    pub id: String,
    pub name: String,
    pub usage: String,
    pub description: String,
    #[serde(default)]
    pub hint: String,
    pub source: String,
    pub trust: PlatformTrust,
    pub effect: UiCommandEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolPresentationKind {
    Generic,
    Command,
    File,
    Search,
    Resource,
}

/// Declarative presentation metadata. Clients interpret this data using their
/// own trusted renderers; extensions never receive rendering callbacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolPresentationDescriptor {
    pub tool_name: String,
    pub label: String,
    pub kind: ToolPresentationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_field: Option<String>,
    pub source: String,
    pub trust: PlatformTrust,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlatformSnapshot {
    #[serde(default)]
    pub features: Vec<PlatformFeature>,
    #[serde(default)]
    pub commands: Vec<UiCommandDescriptor>,
    #[serde(default)]
    pub tool_presentations: Vec<ToolPresentationDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextSourceKind {
    SystemPrompt,
    Conversation,
    Tools,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContextSource {
    pub kind: ContextSourceKind,
    pub label: String,
    pub items: usize,
}

/// Last provider-confirmed context usage plus its structural contributors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContextReport {
    pub model: String,
    pub context_window: u32,
    pub used_tokens: u32,
    pub remaining_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub sources: Vec<ContextSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompactionReport {
    pub automatic: bool,
    pub removed_messages: usize,
    pub condensed_blocks: usize,
    pub freed_tokens: u32,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceRollbackPreview {
    pub turn_id: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceRollbackReport {
    pub turn_id: String,
    pub restored: Vec<String>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RetryCause {
    RateLimit,
    Server,
    Network,
    Stream,
    #[default]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InteractionTimeoutKind {
    Approval,
    Question,
    Plan,
    Tool,
    Task,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutRecovery {
    RetryRequest,
    NarrowScope,
    ContinueWithout,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FileAccess {
    None,
    ReadOnly,
    #[default]
    WorkspaceWrite,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAccess {
    #[default]
    Denied,
    Allowed,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
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
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    #[default]
    Once,
    Session,
    Persistent,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PermissionProfile {
    pub file_access: FileAccess,
    pub network_access: NetworkAccess,
    pub approval_policy: ApprovalPolicy,
}

// ===========================================================================
// ID types
// ===========================================================================

/// Unique identifier for an agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionMetadata {
    pub workspace: PathBuf,
    pub name: String,
    pub user_id: String,
}

/// A workspace exposed to an Agent through a named execution target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionWorkspaceBinding {
    pub execution_target: String,
    pub path: PathBuf,
    #[serde(default)]
    pub read_only: bool,
    /// Relative directory whose ancestor chain supplies workspace
    /// instructions. File tools remain rooted at `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction_focus: Option<PathBuf>,
}

/// Semantic role of one workspace in the Agent's composed filesystem view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMountRole {
    AgentHome,
    Task,
    Dependency,
    Artifact,
}

/// Operations that may be routed to one logical workspace mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCapabilityPolicy {
    #[serde(default = "default_true")]
    pub read: bool,
    #[serde(default)]
    pub write: bool,
    #[serde(default)]
    pub command: bool,
    #[serde(default)]
    pub git: bool,
}

const fn default_true() -> bool {
    true
}

impl Default for WorkspaceCapabilityPolicy {
    fn default() -> Self {
        Self {
            read: true,
            write: false,
            command: false,
            git: false,
        }
    }
}

/// One collision-free logical reference in the effective workspace set.
///
/// File-oriented tools address non-task mounts with `@reference/path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionWorkspaceMount {
    pub reference: String,
    pub role: WorkspaceMountRole,
    pub binding: SessionWorkspaceBinding,
    #[serde(default)]
    pub capabilities: WorkspaceCapabilityPolicy,
}

/// The configuration layer that supplied one effective session field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionConfigSourceKind {
    AgentDefault,
    ChannelDefault,
    SessionOverride,
    RequestOverride,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionConfigSource {
    pub kind: SessionConfigSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// Durable, user-controlled session overrides. Missing fields inherit from
/// the Agent and channel definitions instead of copying their current values.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionConfigOverrides {
    /// Provider-qualified model selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<PermissionProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_workspace: Option<SessionWorkspaceBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_target: Option<String>,
}

impl std::fmt::Debug for SessionConfigOverrides {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionConfigOverrides")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("permissions", &self.permissions)
            .field("prompt_profile", &self.prompt_profile)
            .field(
                "system_prompt",
                &self.system_prompt.as_ref().map(|_| "[REDACTED]"),
            )
            .field("user_workspace", &self.user_workspace)
            .field("execution_target", &self.execution_target)
            .finish()
    }
}

/// Read-only public projection of sparse overrides. Prompt input is write-only;
/// its digest and size remain inspectable through the effective manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactedSessionConfigOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<PermissionProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_workspace: Option<SessionWorkspaceBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_target: Option<String>,
}

impl From<&SessionConfigOverrides> for RedactedSessionConfigOverrides {
    fn from(value: &SessionConfigOverrides) -> Self {
        Self {
            model: value.model.clone(),
            reasoning_effort: value.reasoning_effort,
            permissions: value.permissions.clone(),
            prompt_profile: value.prompt_profile.clone(),
            user_workspace: value.user_workspace.clone(),
            execution_target: value.execution_target.clone(),
        }
    }
}

fn serialize_redacted_session_overrides<S>(
    value: &SessionConfigOverrides,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    RedactedSessionConfigOverrides::from(value).serialize(serializer)
}

impl SessionConfigOverrides {
    /// Resolve the provider-qualified override against the visible catalog.
    pub fn resolve_model_selection(
        &self,
        catalog: &[ModelSelection],
    ) -> Result<Option<ModelSelection>, ModelSelectionResolutionError> {
        match &self.model {
            Some(selection) => {
                let matches = catalog
                    .iter()
                    .filter(|candidate| *candidate == selection)
                    .count();
                if matches == 1 {
                    Ok(Some(selection.clone()))
                } else {
                    Err(ModelSelectionResolutionError::Unavailable {
                        provider_id: selection.provider_id.clone(),
                        model_id: selection.model_id.clone(),
                    })
                }
            }
            None => Ok(None),
        }
    }
}

/// Per-field origin information for the resolved configuration. This keeps UI
/// inspection and audit output honest when a session overrides Agent defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionConfigProvenance {
    pub model: SessionConfigSource,
    pub reasoning_effort: SessionConfigSource,
    pub permissions: SessionConfigSource,
    pub prompt_profile: SessionConfigSource,
    pub system_prompt: SessionConfigSource,
    pub agent_workspace: SessionConfigSource,
    pub user_workspace: SessionConfigSource,
    pub execution_target: SessionConfigSource,
}

/// Immutable registry revisions required before a session may execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionRevisionPins {
    pub provider_revision: u64,
    pub model_revision: u64,
}

/// Stable role of one prompt layer in the exact order used for composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PromptLayerKind {
    SharedSafety,
    ProviderModelProfile,
    Agent,
    SessionInput,
}

/// Content-free digest for one prompt layer. `reference` identifies a public
/// profile or definition revision; it must never contain prompt text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptLayerDigest {
    pub kind: PromptLayerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub sha256: String,
    pub byte_count: u64,
}

/// Ordered, content-free manifest of the effective prompt composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptManifest {
    pub layers: Vec<PromptLayerDigest>,
    pub aggregate_sha256: String,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionRevisionPinError {
    #[error("session Provider revision must be greater than zero")]
    ZeroProviderRevision,
    #[error("session Model revision must be greater than zero")]
    ZeroModelRevision,
}

/// Fully resolved configuration used to start a turn. The runtime persists
/// this value before provider or tool work begins, so later configuration
/// changes cannot rewrite the historical execution context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionEffectiveConfig {
    pub agent_id: AgentId,
    pub agent_revision: u64,
    pub provider_id: String,
    /// Immutable Provider registry revision.
    pub provider_revision: u64,
    pub model_id: String,
    /// Immutable Model registry revision.
    pub model_revision: u64,
    pub reasoning_effort: ReasoningEffort,
    pub permissions: PermissionProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_profile: Option<String>,
    /// Digest of the resolved prompt, never the prompt or credentials.
    pub system_prompt_sha256: String,
    /// Ordered, content-free provenance for the exact composed prompt.
    pub prompt_manifest: PromptManifest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_workspace: Option<SessionWorkspaceBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_workspace: Option<SessionWorkspaceBinding>,
    /// Canonical role-bearing workspace composition. The singular fields above
    /// remain projections for the default Agent-home and task bindings.
    #[serde(default)]
    pub workspace_mounts: Vec<SessionWorkspaceMount>,
    pub execution_target: String,
    pub provenance: SessionConfigProvenance,
}

impl SessionEffectiveConfig {
    #[must_use]
    pub fn model_selection(&self) -> ModelSelection {
        ModelSelection {
            provider_id: self.provider_id.clone(),
            model_id: self.model_id.clone(),
        }
    }

    /// Return execution-safe revision pins, rejecting the reserved zero
    /// revision.
    pub fn require_revision_pins(&self) -> Result<SessionRevisionPins, SessionRevisionPinError> {
        if self.provider_revision == 0 {
            return Err(SessionRevisionPinError::ZeroProviderRevision);
        }
        if self.model_revision == 0 {
            return Err(SessionRevisionPinError::ZeroModelRevision);
        }
        Ok(SessionRevisionPins {
            provider_revision: self.provider_revision,
            model_revision: self.model_revision,
        })
    }
}

/// Redacted Agent definition exposed to UI clients during discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentDescriptor {
    pub id: AgentId,
    pub revision: u64,
    pub name: String,
    pub provider_id: String,
    pub default_model_id: String,
    #[serde(default)]
    pub models: Vec<ModelDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_prompt_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_workspace: Option<SessionWorkspaceBinding>,
}

/// UI-facing request to create a durable session from layered defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionCreateRequest {
    pub agent_id: AgentId,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub overrides: SessionConfigOverrides,
}

/// Optimistic UI request to replace one session's sparse overrides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionConfigUpdateRequest {
    pub session_id: SessionId,
    pub expected_revision: u64,
    pub overrides: SessionConfigOverrides,
}

/// Complete session configuration state returned after create, read, or update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionConfigState {
    pub session_id: SessionId,
    pub revision: u64,
    #[serde(serialize_with = "serialize_redacted_session_overrides")]
    #[schemars(with = "RedactedSessionConfigOverrides")]
    pub overrides: SessionConfigOverrides,
    pub effective: SessionEffectiveConfig,
}

/// A user assessment tied to durable execution evidence, never free-floating
/// training data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackRating {
    Positive,
    Negative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackTaskResult {
    Succeeded,
    Failed,
    Partial,
    Cancelled,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackPrivacyClass {
    MetadataOnly,
    #[default]
    Private,
    Shareable,
}

/// Opaque, server-issued handle for one durable execution turn.
///
/// Clients must preserve this value verbatim. The wire contract deliberately
/// does not expose Runtime run or turn identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct FeedbackTarget(pub String);

impl FeedbackTarget {
    /// Return whether this value has the exact server-issued digest shape.
    ///
    /// This validates framing only; Runtime must still resolve the target and
    /// authorize the owning session before accepting feedback.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.0.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReference {
    pub locator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunFeedback {
    pub target: FeedbackTarget,
    pub rating: FeedbackRating,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_result: Option<FeedbackTaskResult>,
    #[serde(default)]
    pub artifacts: Vec<EvidenceReference>,
    #[serde(default)]
    pub validations: Vec<EvidenceReference>,
    #[serde(default)]
    pub privacy_class: FeedbackPrivacyClass,
}

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
#[path = "../tests/unit/types.rs"]
mod tests;
