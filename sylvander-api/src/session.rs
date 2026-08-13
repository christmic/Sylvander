//! Durable Session configuration and redacted public state DTOs.
//!
//! Runtime owns Session lifecycle and persistence. This module defines only
//! the versioned values that cross the service boundary, including sparse
//! overrides, immutable effective configuration, provenance, and workspace
//! references.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::identity::{AgentId, SessionId};
use crate::{
    ModelDescriptor, ModelSelection, ModelSelectionResolutionError, PermissionProfile,
    ReasoningEffort,
};

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

/// One explicit mutation of a sparse Session override.
///
/// `Inherit` removes the durable override, while `Set` replaces it. Omitting
/// the containing patch field preserves its current value. The three states
/// are intentionally distinct so write-only values are never cleared by a
/// read-modify-write client that cannot observe them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionConfigFieldPatch<T> {
    Inherit,
    Set { value: T },
}

impl<T> SessionConfigFieldPatch<T> {
    fn apply(self, target: &mut Option<T>) {
        *target = match self {
            Self::Inherit => None,
            Self::Set { value } => Some(value),
        };
    }
}

/// Field-level update for durable Session overrides.
///
/// A missing field means “preserve”, not “inherit”. This makes optimistic
/// updates safe even though public reads redact the write-only system prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<SessionConfigFieldPatch<ModelSelection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<SessionConfigFieldPatch<ReasoningEffort>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<SessionConfigFieldPatch<PermissionProfile>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_profile: Option<SessionConfigFieldPatch<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<SessionConfigFieldPatch<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_workspace: Option<SessionConfigFieldPatch<SessionWorkspaceBinding>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_target: Option<SessionConfigFieldPatch<String>>,
}

impl SessionConfigPatch {
    /// Apply only the fields present in this patch to the durable overrides.
    pub fn apply_to(self, overrides: &mut SessionConfigOverrides) {
        if let Some(patch) = self.model {
            patch.apply(&mut overrides.model);
        }
        if let Some(patch) = self.reasoning_effort {
            patch.apply(&mut overrides.reasoning_effort);
        }
        if let Some(patch) = self.permissions {
            patch.apply(&mut overrides.permissions);
        }
        if let Some(patch) = self.prompt_profile {
            patch.apply(&mut overrides.prompt_profile);
        }
        if let Some(patch) = self.system_prompt {
            patch.apply(&mut overrides.system_prompt);
        }
        if let Some(patch) = self.user_workspace {
            patch.apply(&mut overrides.user_workspace);
        }
        if let Some(patch) = self.execution_target {
            patch.apply(&mut overrides.execution_target);
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
    pub patch: SessionConfigPatch,
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

#[cfg(test)]
#[path = "../tests/unit/session.rs"]
mod tests;
