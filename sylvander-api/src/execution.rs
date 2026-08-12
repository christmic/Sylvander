//! Redacted execution policy, progress, and recovery DTOs.
//!
//! Runtime publishes these values for inspection and interaction. They describe
//! decisions and outcomes only; executable authority and sandbox handles are
//! deliberately absent from the wire contract.

use serde::{Deserialize, Serialize};

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
