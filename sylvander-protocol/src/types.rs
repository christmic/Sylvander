//! Wire-format data types — cross-language definitions.
//!
//! Every type here has `serde::Serialize/Deserialize` and
//! `schemars::JsonSchema` derives. The JSON Schema output is the
//! basis for TypeScript, Python, Swift, etc. code generation.

use serde::{Deserialize, Serialize};

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

#[cfg(test)]
#[path = "../tests/unit/types.rs"]
mod tests;
