//! Transport-neutral administration contract for immutable Agent revisions.
//!
//! Write DTOs are deliberately separate from inspection DTOs. Inspection
//! never returns prompts, command templates, process arguments, environment
//! bindings, workspace paths, or secret references.

use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};

use crate::{AgentId, ModelSelection, SessionWorkspaceBinding, SessionWorkspaceMount};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentAdminRequest {
    InspectRevision {
        agent_id: AgentId,
        revision: u64,
    },
    ListRevisions {
        agent_id: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_revision: Option<u64>,
        #[serde(default = "default_revision_page_size")]
        limit: u16,
    },
    UpdateDefinition {
        expected_active_revision: u64,
        definition: Box<AgentDefinitionDraft>,
    },
    ActivateRevision {
        agent_id: AgentId,
        revision: u64,
        expected_active_revision: u64,
    },
    RollbackRevision {
        agent_id: AgentId,
        target_revision: u64,
        expected_active_revision: u64,
    },
}

const fn default_revision_page_size() -> u16 {
    50
}

/// Complete, validated candidate supplied by a privileged administrator.
/// Secret-bearing process environment entries are references, never values.
#[derive(Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinitionDraft {
    pub agent_id: AgentId,
    pub revision: u64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub provider_id: String,
    pub default_model_id: String,
    /// Provider-qualified models that sessions may select, including the
    /// Agent default. The exact, non-empty allowlist is required on every
    /// definition write.
    pub allowed_models: Vec<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Write-only in the public contract.
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub tools: Vec<AgentToolDraft>,
    #[serde(default)]
    pub memory_stores: Vec<AgentMemoryStoreDraft>,
    #[serde(default)]
    pub ui_commands: Vec<AgentUiCommandDraft>,
    #[serde(default)]
    pub hooks: Vec<AgentHookDraft>,
    #[serde(default)]
    pub tool_presentations: Vec<AgentToolPresentationDraft>,
    #[serde(default)]
    pub behavior: AgentBehaviorDraft,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_workspace: Option<SessionWorkspaceBinding>,
    #[serde(default)]
    pub workspace_mounts: Vec<SessionWorkspaceMount>,
    #[serde(default)]
    pub prompt_profiles: Vec<AgentPromptProfileDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_prompt_profile: Option<String>,
    #[serde(default)]
    pub allow_session_prompt: bool,
    #[serde(default)]
    pub access: AgentAccessDraft,
}

impl fmt::Debug for AgentDefinitionDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentDefinitionDraft")
            .field("agent_id", &self.agent_id)
            .field("revision", &self.revision)
            .field("name", &self.name)
            .field("provider_id", &self.provider_id)
            .field("default_model_id", &self.default_model_id)
            .field("allowed_model_count", &self.allowed_models.len())
            .field("system_prompt", &"[REDACTED]")
            .field("tool_count", &self.tools.len())
            .field("memory_store_count", &self.memory_stores.len())
            .field("ui_command_count", &self.ui_commands.len())
            .field("hook_count", &self.hooks.len())
            .field("tool_presentation_count", &self.tool_presentations.len())
            .field("prompt_profile_count", &self.prompt_profiles.len())
            .field(
                "agent_workspace_configured",
                &self.agent_workspace.is_some(),
            )
            .field("workspace_mount_count", &self.workspace_mounts.len())
            .field("default_prompt_profile", &self.default_prompt_profile)
            .field("allow_session_prompt", &self.allow_session_prompt)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentToolDraft {
    Builtin {
        name: String,
    },
    McpServer {
        name: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        environment: BTreeMap<String, AgentSecretReference>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentSecretReference {
    Environment { name: String },
    File { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentMemoryStoreDraft {
    pub store_type: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentUiCommandDraft {
    pub id: String,
    pub name: String,
    pub usage: String,
    pub description: String,
    #[serde(default)]
    pub hint: String,
    /// Write-only in the public contract.
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentHookDraft {
    pub name: String,
    /// Exact lifecycle boundary at which this command executes.
    ///
    /// This field is intentionally required. The current protocol exposes only
    /// phases that have a production execution path; unknown, session-level,
    /// or future phases fail deserialization instead of becoming inert config.
    pub phase: AgentHookPhase,
    pub command: String,
    #[serde(default = "default_hook_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub blocking: bool,
}

/// Production hook lifecycle boundaries accepted by the latest Agent schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentHookPhase {
    BeforeTool,
    AfterTool,
    BeforeTurn,
    AfterTurn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentToolPresentationDraft {
    pub tool_name: String,
    pub label: String,
    pub kind: crate::ToolPresentationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_field: Option<String>,
}

const fn default_hook_timeout_secs() -> u64 {
    30
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentPromptProfileDraft {
    pub id: String,
    /// Exact Provider/Model selectors. An empty list applies to every model.
    #[serde(default)]
    pub qualified_models: Vec<ModelSelection>,
    /// Write-only in the public contract.
    pub system_prompt: String,
}

impl fmt::Debug for AgentPromptProfileDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentPromptProfileDraft")
            .field("id", &self.id)
            .field("qualified_model_count", &self.qualified_models.len())
            .field("system_prompt", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AgentBehaviorDraft {
    pub max_iterations: u32,
    pub max_retries: u32,
}

impl Default for AgentBehaviorDraft {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            max_retries: 3,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AgentAccessDraft {
    pub allow_authenticated: bool,
    pub allowed_principals: Vec<String>,
    pub allowed_roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentAdminResponse {
    Success { result: Box<AgentAdminResult> },
    Error { error: AgentAdminError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentAdminResult {
    RevisionInspected {
        revision: AgentRevisionView,
    },
    RevisionsListed {
        agent_id: AgentId,
        active_revision: u64,
        revisions: Vec<AgentRevisionView>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_before_revision: Option<u64>,
    },
    DefinitionUpdated {
        revision: AgentRevisionView,
    },
    RevisionActivated {
        agent_id: AgentId,
        active_revision: u64,
    },
    RevisionRolledBack {
        agent_id: AgentId,
        active_revision: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentRevisionView {
    pub definition: RedactedAgentDefinition,
    pub digest_sha256: String,
    pub created_at_unix_secs: i64,
    pub active: bool,
}

/// Safe inspection surface. Sensitive definition fields are represented only
/// by digests, counts, or boolean presence markers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactedAgentDefinition {
    pub agent_id: AgentId,
    pub revision: u64,
    pub name: String,
    pub description: String,
    pub provider_id: String,
    pub default_model_id: String,
    /// Non-sensitive provider-qualified session model allowlist.
    #[serde(default)]
    pub allowed_models: Vec<ModelSelection>,
    pub system_prompt_sha256: String,
    #[serde(default)]
    pub tools: Vec<RedactedAgentTool>,
    #[serde(default)]
    pub memory_store_types: Vec<String>,
    #[serde(default)]
    pub ui_commands: Vec<RedactedAgentUiCommand>,
    #[serde(default)]
    pub hooks: Vec<RedactedAgentHook>,
    #[serde(default)]
    pub tool_presentations: Vec<AgentToolPresentationDraft>,
    pub behavior: AgentBehaviorDraft,
    pub agent_workspace_configured: bool,
    pub workspace_mount_count: usize,
    #[serde(default)]
    pub prompt_profiles: Vec<RedactedAgentPromptProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_prompt_profile: Option<String>,
    pub allow_session_prompt: bool,
    pub access: RedactedAgentAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RedactedAgentTool {
    Builtin { name: String },
    McpServer { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactedAgentUiCommand {
    pub id: String,
    pub name: String,
    pub usage: String,
    pub description: String,
    #[serde(default)]
    pub hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactedAgentHook {
    pub name: String,
    pub phase: AgentHookPhase,
    pub timeout_secs: u64,
    pub blocking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactedAgentPromptProfile {
    pub id: String,
    #[serde(default)]
    pub qualified_models: Vec<ModelSelection>,
    pub system_prompt_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactedAgentAccess {
    pub allow_authenticated: bool,
    pub allowed_principal_count: u32,
    #[serde(default)]
    pub allowed_roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentAdminError {
    pub code: AgentAdminErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_active_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_active_revision: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentAdminErrorCode {
    Unauthorized,
    InvalidDefinition,
    UnknownAgent,
    UnknownRevision,
    RevisionConflict,
    NonSequentialRevision,
    RevisionCollision,
    InvalidRollback,
    StorageUnavailable,
    Internal,
}

#[cfg(test)]
#[path = "../tests/unit/agent_admin.rs"]
mod tests;
