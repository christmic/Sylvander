//! Redacted optional-platform capability and presentation DTOs.
//!
//! These declarations let clients render trusted Runtime state without
//! exposing credentials, command arguments, filesystem paths, or callbacks.

use serde::{Deserialize, Serialize};

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

#[cfg(test)]
#[path = "../tests/unit/platform.rs"]
mod tests;
