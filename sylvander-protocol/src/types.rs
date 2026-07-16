//! Wire-format data types — cross-language definitions.
//!
//! Every type here has `serde::Serialize/Deserialize` and
//! `schemars::JsonSchema` derives. The JSON Schema output is the
//! basis for TypeScript, Python, Swift, etc. code generation.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const UI_PROTOCOL_MIN_VERSION: u16 = 1;
pub const UI_PROTOCOL_MAX_VERSION: u16 = 3;

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
    /// Legacy bitset retained for wire compatibility with UI protocol v1-v3.
    pub capabilities: u8,
    /// Provider-neutral, canonical capabilities for current clients.
    #[serde(default)]
    pub capability_names: Vec<ModelCapability>,
    pub reasoning_efforts: Vec<ReasoningEffort>,
    #[serde(default)]
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

/// Backward-compatible model selection accepted by public UI requests.
///
/// Current clients send a provider-qualified object. Legacy clients may keep
/// sending a bare model id, which the server must resolve against the visible
/// catalog before it mutates session configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ModelSelectionInput {
    Qualified(ModelSelection),
    Legacy(String),
}

impl From<String> for ModelSelectionInput {
    fn from(model_id: String) -> Self {
        Self::Legacy(model_id)
    }
}

impl From<&str> for ModelSelectionInput {
    fn from(model_id: &str) -> Self {
        Self::Legacy(model_id.to_owned())
    }
}

impl ModelSelectionInput {
    /// Resolve one public input without guessing when a legacy id is absent
    /// or shared by more than one provider.
    pub fn resolve(
        &self,
        catalog: &[ModelSelection],
    ) -> Result<ModelSelection, ModelSelectionResolutionError> {
        match self {
            Self::Qualified(selection) => catalog
                .iter()
                .find(|candidate| *candidate == selection)
                .cloned()
                .ok_or_else(|| ModelSelectionResolutionError::Unavailable {
                    provider_id: selection.provider_id.clone(),
                    model_id: selection.model_id.clone(),
                }),
            Self::Legacy(model_id) => {
                let matches = catalog
                    .iter()
                    .filter(|candidate| candidate.model_id == *model_id)
                    .collect::<Vec<_>>();
                match matches.as_slice() {
                    [] => Err(ModelSelectionResolutionError::LegacyUnavailable {
                        model_id: model_id.clone(),
                    }),
                    [selection] => Ok((*selection).clone()),
                    _ => Err(ModelSelectionResolutionError::LegacyAmbiguous {
                        model_id: model_id.clone(),
                        provider_ids: matches
                            .iter()
                            .map(|selection| selection.provider_id.clone())
                            .collect(),
                    }),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelSelectionResolutionError {
    #[error("model and legacy model_id cannot both be set")]
    ConflictingOverrides,
    #[error("model selection `{provider_id}/{model_id}` is unavailable")]
    Unavailable {
        provider_id: String,
        model_id: String,
    },
    #[error("legacy model id `{model_id}` is unavailable")]
    LegacyUnavailable { model_id: String },
    #[error("legacy model id `{model_id}` is ambiguous across providers: {provider_ids:?}")]
    LegacyAmbiguous {
        model_id: String,
        provider_ids: Vec<String>,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlatformSnapshot {
    #[serde(default)]
    pub features: Vec<PlatformFeature>,
    #[serde(default)]
    pub commands: Vec<UiCommandDescriptor>,
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
}

/// The configuration layer that supplied one effective session field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionConfigSourceKind {
    AgentDefault,
    ChannelDefault,
    SessionOverride,
    RequestOverride,
    LegacyMigration,
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
#[serde(try_from = "SessionConfigOverridesWire")]
pub struct SessionConfigOverrides {
    /// Provider-qualified model selection used by current clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
    /// Legacy model-only selection accepted solely for catalog-aware
    /// migration. New clients must write `model` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
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
            .field("model_id", &self.model_id)
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
    pub model_id: Option<String>,
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
            model_id: value.model_id.clone(),
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

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "SessionConfigOverrides")]
struct SessionConfigOverridesWire {
    /// Provider-qualified model selection used by current clients.
    #[serde(default)]
    model: Option<ModelSelection>,
    /// Legacy model-only selection accepted for catalog-aware migration.
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    permissions: Option<PermissionProfile>,
    #[serde(default)]
    prompt_profile: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    user_workspace: Option<SessionWorkspaceBinding>,
    #[serde(default)]
    execution_target: Option<String>,
}

impl TryFrom<SessionConfigOverridesWire> for SessionConfigOverrides {
    type Error = ModelSelectionResolutionError;

    fn try_from(wire: SessionConfigOverridesWire) -> Result<Self, Self::Error> {
        if wire.model.is_some() && wire.model_id.is_some() {
            return Err(ModelSelectionResolutionError::ConflictingOverrides);
        }
        Ok(Self {
            model: wire.model,
            model_id: wire.model_id,
            reasoning_effort: wire.reasoning_effort,
            permissions: wire.permissions,
            prompt_profile: wire.prompt_profile,
            system_prompt: wire.system_prompt,
            user_workspace: wire.user_workspace,
            execution_target: wire.execution_target,
        })
    }
}

impl SessionConfigOverrides {
    /// Resolve a provider-qualified selection from a current or legacy
    /// override. A legacy id migrates only when it uniquely identifies one
    /// catalog entry; missing and ambiguous ids fail closed.
    pub fn resolve_model_selection(
        &self,
        catalog: &[ModelSelection],
    ) -> Result<Option<ModelSelection>, ModelSelectionResolutionError> {
        match (&self.model, &self.model_id) {
            (Some(_), Some(_)) => Err(ModelSelectionResolutionError::ConflictingOverrides),
            (Some(selection), None) => {
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
            (None, Some(legacy_model_id)) => ModelSelectionInput::Legacy(legacy_model_id.clone())
                .resolve(catalog)
                .map(Some),
            (None, None) => Ok(None),
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

/// A legacy effective configuration can be decoded without revision pins, but
/// it cannot execute until the runtime resolves and persists both revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionRevisionPinError {
    #[error("session Provider revision is missing")]
    MissingProviderRevision,
    #[error("session Provider revision must be greater than zero")]
    ZeroProviderRevision,
    #[error("session Model revision is missing")]
    MissingModelRevision,
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
    /// Immutable Provider registry revision. `None` is accepted only while
    /// loading a legacy session that still requires migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_revision: Option<u64>,
    pub model_id: String,
    /// Immutable Model registry revision. `None` is accepted only while
    /// loading a legacy session that still requires migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_revision: Option<u64>,
    pub reasoning_effort: ReasoningEffort,
    pub permissions: PermissionProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_profile: Option<String>,
    /// Digest of the resolved prompt, never the prompt or credentials.
    pub system_prompt_sha256: String,
    /// Optional for backward compatibility with sessions created before
    /// prompt-layer provenance was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_manifest: Option<PromptManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_workspace: Option<SessionWorkspaceBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_workspace: Option<SessionWorkspaceBinding>,
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

    /// Return execution-safe revision pins, rejecting unresolved legacy data
    /// and the reserved zero revision.
    pub fn require_revision_pins(&self) -> Result<SessionRevisionPins, SessionRevisionPinError> {
        let provider_revision = self
            .provider_revision
            .ok_or(SessionRevisionPinError::MissingProviderRevision)?;
        if provider_revision == 0 {
            return Err(SessionRevisionPinError::ZeroProviderRevision);
        }
        let model_revision = self
            .model_revision
            .ok_or(SessionRevisionPinError::MissingModelRevision)?;
        if model_revision == 0 {
            return Err(SessionRevisionPinError::ZeroModelRevision);
        }
        Ok(SessionRevisionPins {
            provider_revision,
            model_revision,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RunFeedback {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub rating: FeedbackRating,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
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
        #[serde(default)]
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

fn default_approval_scopes() -> Vec<ApprovalScope> {
    vec![ApprovalScope::Once]
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
mod tests {
    use super::*;

    fn effective_config_json() -> serde_json::Value {
        let source = serde_json::json!({ "kind": "agent_default" });
        serde_json::json!({
            "agent_id": "agent-1",
            "agent_revision": 3,
            "provider_id": "provider-1",
            "model_id": "model-1",
            "reasoning_effort": "off",
            "permissions": {
                "file_access": "workspace_write",
                "network_access": "denied",
                "approval_policy": "allow"
            },
            "system_prompt_sha256": "digest",
            "execution_target": "local",
            "provenance": {
                "model": source.clone(),
                "reasoning_effort": source.clone(),
                "permissions": source.clone(),
                "prompt_profile": source.clone(),
                "system_prompt": source.clone(),
                "agent_workspace": source.clone(),
                "user_workspace": source.clone(),
                "execution_target": source
            }
        })
    }

    #[test]
    fn legacy_effective_config_omits_unresolved_revision_pins() {
        let config: SessionEffectiveConfig =
            serde_json::from_value(effective_config_json()).expect("legacy config");
        assert_eq!(config.provider_revision, None);
        assert_eq!(config.model_revision, None);
        assert_eq!(config.prompt_manifest, None);
        assert_eq!(
            config.require_revision_pins(),
            Err(SessionRevisionPinError::MissingProviderRevision)
        );

        let encoded = serde_json::to_value(config).expect("serialize legacy config");
        assert!(encoded.get("provider_revision").is_none());
        assert!(encoded.get("model_revision").is_none());
        assert!(encoded.get("prompt_manifest").is_none());
    }

    #[test]
    fn prompt_manifest_round_trips_in_composition_order() {
        let mut json = effective_config_json();
        json["prompt_manifest"] = serde_json::json!({
            "layers": [
                {
                    "kind": "shared_safety",
                    "reference": "safety-v2",
                    "sha256": "aaa",
                    "byte_count": 12
                },
                {
                    "kind": "agent",
                    "reference": "agent-1@3",
                    "sha256": "bbb",
                    "byte_count": 34
                },
                {
                    "kind": "session_input",
                    "sha256": "ccc",
                    "byte_count": 5
                }
            ],
            "aggregate_sha256": "aggregate",
            "total_bytes": 51
        });

        let config: SessionEffectiveConfig = serde_json::from_value(json).unwrap();
        let manifest = config.prompt_manifest.as_ref().unwrap();
        assert_eq!(manifest.layers[0].kind, PromptLayerKind::SharedSafety);
        assert_eq!(manifest.layers[1].kind, PromptLayerKind::Agent);
        assert_eq!(manifest.layers[2].kind, PromptLayerKind::SessionInput);
        assert_eq!(manifest.total_bytes, 51);
        let expected_manifest = manifest.clone();

        let round_trip: SessionEffectiveConfig =
            serde_json::from_value(serde_json::to_value(config).unwrap()).unwrap();
        assert_eq!(round_trip.prompt_manifest.unwrap(), expected_manifest);
    }

    #[test]
    fn session_config_state_keeps_prompt_input_write_only() {
        let mut effective_json = effective_config_json();
        effective_json["prompt_manifest"] = serde_json::json!({
            "layers": [{
                "kind": "session_input",
                "reference": "session",
                "sha256": "session-digest",
                "byte_count": 24
            }],
            "aggregate_sha256": "aggregate",
            "total_bytes": 24
        });
        let state = SessionConfigState {
            session_id: SessionId::new("session-1"),
            revision: 2,
            overrides: SessionConfigOverrides {
                prompt_profile: Some("coding".into()),
                system_prompt: Some("private session sentinel".into()),
                ..SessionConfigOverrides::default()
            },
            effective: serde_json::from_value(effective_json).unwrap(),
        };
        let debug = format!("{:?}", state.overrides);
        assert!(!debug.contains("private session sentinel"));

        let encoded = serde_json::to_value(&state).unwrap();
        assert!(!encoded.to_string().contains("private session sentinel"));
        assert!(encoded["overrides"].get("system_prompt").is_none());
        assert_eq!(
            encoded["effective"]["prompt_manifest"]["layers"][0]["sha256"],
            "session-digest"
        );
        let decoded: SessionConfigState = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.overrides.prompt_profile.as_deref(), Some("coding"));
        assert!(decoded.overrides.system_prompt.is_none());
    }

    #[test]
    fn pinned_effective_config_round_trips_and_validates() {
        let mut json = effective_config_json();
        json["provider_revision"] = serde_json::json!(7);
        json["model_revision"] = serde_json::json!(11);
        let config: SessionEffectiveConfig = serde_json::from_value(json).expect("pinned config");
        assert_eq!(
            config.require_revision_pins(),
            Ok(SessionRevisionPins {
                provider_revision: 7,
                model_revision: 11,
            })
        );
        let round_trip: SessionEffectiveConfig =
            serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
        assert_eq!(round_trip, config);
    }

    #[test]
    fn revision_pin_validation_rejects_each_missing_or_zero_value() {
        let mut json = effective_config_json();
        json["provider_revision"] = serde_json::json!(0);
        json["model_revision"] = serde_json::json!(1);
        let config: SessionEffectiveConfig = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            config.require_revision_pins(),
            Err(SessionRevisionPinError::ZeroProviderRevision)
        );

        json["provider_revision"] = serde_json::json!(1);
        json.as_object_mut().unwrap().remove("model_revision");
        let config: SessionEffectiveConfig = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            config.require_revision_pins(),
            Err(SessionRevisionPinError::MissingModelRevision)
        );

        json["model_revision"] = serde_json::json!(0);
        let config: SessionEffectiveConfig = serde_json::from_value(json).unwrap();
        assert_eq!(
            config.require_revision_pins(),
            Err(SessionRevisionPinError::ZeroModelRevision)
        );
    }

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
                reason: None,
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
    fn approval_rejection_reason_round_trips_without_transport_semantics() {
        let system = SystemMessage::ApproveTool {
            call_id: "call-1".into(),
            approved: false,
            scope: ApprovalScope::Once,
            reason: Some("unsafe outside workspace".into()),
        };
        let json = serde_json::to_value(&system).expect("serialize approval");
        let decoded: SystemMessage = serde_json::from_value(json).expect("decode approval");
        assert_eq!(decoded, system);
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
    fn legacy_model_descriptors_default_new_metadata() {
        let descriptor: ModelDescriptor = serde_json::from_value(serde_json::json!({
            "id": "model-a",
            "provider": "test",
            "capabilities": 0,
            "reasoning_efforts": ["off"]
        }))
        .expect("legacy model descriptor");
        assert!(descriptor.capability_names.is_empty());
        assert_eq!(descriptor.lifecycle, ModelLifecycle::Active);
        assert_eq!(descriptor.pricing, None);
    }

    #[test]
    fn model_capability_names_are_canonical_and_strict() {
        let descriptor: ModelDescriptor = serde_json::from_value(serde_json::json!({
            "id": "model-a",
            "provider": "test",
            "capabilities": 8,
            "capability_names": ["tool_use", "vision"],
            "reasoning_efforts": ["off"]
        }))
        .expect("canonical capability names");
        assert_eq!(
            descriptor.capability_names,
            [ModelCapability::ToolUse, ModelCapability::Vision]
        );
        assert!(
            serde_json::from_value::<ModelDescriptor>(serde_json::json!({
                "id": "model-a",
                "provider": "test",
                "capabilities": 0,
                "capability_names": ["telepathy"],
                "reasoning_efforts": ["off"]
            }))
            .is_err()
        );
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
            commands: vec![UiCommandDescriptor {
                id: "review-security".into(),
                name: "security-review".into(),
                usage: "/security-review [scope]".into(),
                description: "Review a selected scope".into(),
                hint: "workspace command".into(),
                source: "agent configuration".into(),
                trust: PlatformTrust::Workspace,
                effect: UiCommandEffect::SubmitPrompt {
                    template: "Review {{args}} for security issues.".into(),
                },
            }],
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let restored: PlatformSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, snapshot);
    }

    #[test]
    fn ui_protocol_selects_overlap_and_rejects_incompatible_ranges() {
        let legacy = UiProtocolHello {
            client_name: "test".into(),
            min_version: 1,
            max_version: 1,
            capabilities: vec!["diagnostics".into()],
        };
        assert_eq!(negotiate_ui_protocol(&legacy), Ok(1));

        let version_two = UiProtocolHello {
            max_version: 2,
            ..legacy.clone()
        };
        assert_eq!(negotiate_ui_protocol(&version_two), Ok(2));

        let current = UiProtocolHello {
            min_version: 3,
            max_version: 3,
            ..legacy.clone()
        };
        assert_eq!(negotiate_ui_protocol(&current), Ok(3));

        let incompatible = UiProtocolHello {
            min_version: 4,
            max_version: 4,
            ..legacy
        };
        let error = negotiate_ui_protocol(&incompatible).expect_err("must reject");
        assert_eq!(error.code, "incompatible_protocol");
        assert_eq!(error.server_max_version, UI_PROTOCOL_MAX_VERSION);
    }

    #[test]
    fn session_config_update_contract_preserves_optimistic_revision() {
        let request = SessionConfigUpdateRequest {
            session_id: SessionId::new("session-1"),
            expected_revision: 7,
            overrides: SessionConfigOverrides {
                model_id: Some("model-b".into()),
                reasoning_effort: Some(ReasoningEffort::High),
                ..SessionConfigOverrides::default()
            },
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["expected_revision"], 7);
        assert_eq!(json["overrides"]["model_id"], "model-b");
        assert_eq!(
            serde_json::from_value::<SessionConfigUpdateRequest>(json).unwrap(),
            request
        );
    }

    fn model(provider_id: &str, model_id: &str) -> ModelSelection {
        ModelSelection {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        }
    }

    #[test]
    fn qualified_model_selection_has_a_stable_schema_and_wire_shape() {
        let selection = model("anthropic", "claude-sonnet");
        assert_eq!(
            serde_json::to_value(&selection).unwrap(),
            serde_json::json!({
                "provider_id": "anthropic",
                "model_id": "claude-sonnet"
            })
        );

        let schema = serde_json::to_value(schemars::schema_for!(ModelSelection)).unwrap();
        assert_eq!(
            schema["required"],
            serde_json::json!(["provider_id", "model_id"])
        );
        assert!(schema["properties"]["provider_id"].is_object());
        assert!(schema["properties"]["model_id"].is_object());
    }

    #[test]
    fn public_model_input_resolves_qualified_and_unique_legacy_models() {
        let catalog = vec![model("anthropic", "shared"), model("openai", "gpt-5")];

        assert_eq!(
            ModelSelectionInput::Qualified(model("openai", "gpt-5")).resolve(&catalog),
            Ok(model("openai", "gpt-5"))
        );
        assert_eq!(
            ModelSelectionInput::Legacy("shared".into()).resolve(&catalog),
            Ok(model("anthropic", "shared"))
        );
        assert!(matches!(
            ModelSelectionInput::Qualified(model("missing", "shared")).resolve(&catalog),
            Err(ModelSelectionResolutionError::Unavailable { .. })
        ));
    }

    #[test]
    fn public_legacy_model_input_fails_closed_when_missing_or_ambiguous() {
        let catalog = vec![model("anthropic", "shared"), model("openai", "shared")];

        assert!(matches!(
            ModelSelectionInput::Legacy("missing".into()).resolve(&catalog),
            Err(ModelSelectionResolutionError::LegacyUnavailable { .. })
        ));
        assert_eq!(
            ModelSelectionInput::Legacy("shared".into()).resolve(&catalog),
            Err(ModelSelectionResolutionError::LegacyAmbiguous {
                model_id: "shared".into(),
                provider_ids: vec!["anthropic".into(), "openai".into()],
            })
        );

        let schema = serde_json::to_string(&schemars::schema_for!(ModelSelectionInput)).unwrap();
        assert!(schema.contains("anyOf"));
        assert!(schema.contains("ModelSelection"));
        assert!(schema.contains(r#""type":"string""#));
    }

    #[test]
    fn current_override_round_trips_a_qualified_model() {
        let overrides = SessionConfigOverrides {
            model: Some(model("openai", "gpt-5")),
            ..SessionConfigOverrides::default()
        };
        let json = serde_json::to_value(&overrides).unwrap();
        assert_eq!(json["model"]["provider_id"], "openai");
        assert!(json.get("model_id").is_none());
        assert_eq!(
            serde_json::from_value::<SessionConfigOverrides>(json).unwrap(),
            overrides
        );
    }

    #[test]
    fn legacy_model_id_migrates_only_on_one_catalog_match() {
        let overrides: SessionConfigOverrides =
            serde_json::from_value(serde_json::json!({ "model_id": "sonnet" })).unwrap();
        let catalog = vec![model("anthropic", "sonnet"), model("openai", "gpt-5")];

        assert_eq!(
            overrides.resolve_model_selection(&catalog),
            Ok(Some(model("anthropic", "sonnet")))
        );
    }

    #[test]
    fn legacy_model_id_fails_closed_when_missing_or_ambiguous() {
        let overrides = SessionConfigOverrides {
            model_id: Some("shared".into()),
            ..SessionConfigOverrides::default()
        };
        assert_eq!(
            overrides.resolve_model_selection(&[]),
            Err(ModelSelectionResolutionError::LegacyUnavailable {
                model_id: "shared".into()
            })
        );

        let catalog = vec![model("provider-a", "shared"), model("provider-b", "shared")];
        assert_eq!(
            overrides.resolve_model_selection(&catalog),
            Err(ModelSelectionResolutionError::LegacyAmbiguous {
                model_id: "shared".into(),
                provider_ids: vec!["provider-a".into(), "provider-b".into()]
            })
        );
    }

    #[test]
    fn override_wire_rejects_qualified_and_legacy_model_together() {
        let error = serde_json::from_value::<SessionConfigOverrides>(serde_json::json!({
            "model": { "provider_id": "anthropic", "model_id": "sonnet" },
            "model_id": "sonnet"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("cannot both be set"));
    }

    #[test]
    fn feedback_requires_a_run_identity_and_has_stable_wire_values() {
        let feedback = RunFeedback {
            run_id: "run-1".into(),
            turn_id: Some("turn-2".into()),
            rating: FeedbackRating::Negative,
            note: Some("tool changed the wrong file".into()),
            tags: vec!["correctness".into()],
        };
        let json = serde_json::to_value(&feedback).unwrap();
        assert_eq!(json["rating"], "negative");
        assert_eq!(json["run_id"], "run-1");
        assert_eq!(
            serde_json::from_value::<RunFeedback>(json).unwrap(),
            feedback
        );
    }
}
