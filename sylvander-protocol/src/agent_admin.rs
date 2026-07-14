//! Transport-neutral administration contract for immutable Agent revisions.
//!
//! Write DTOs are deliberately separate from inspection DTOs. Inspection
//! never returns prompts, command templates, process arguments, environment
//! bindings, workspace paths, or secret references.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{AgentId, SessionWorkspaceBinding};

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinitionDraft {
    pub agent_id: AgentId,
    pub revision: u64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub provider_id: String,
    pub default_model_id: String,
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
    pub behavior: AgentBehaviorDraft,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_workspace: Option<SessionWorkspaceBinding>,
    #[serde(default)]
    pub prompt_profiles: Vec<AgentPromptProfileDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_prompt_profile: Option<String>,
    #[serde(default)]
    pub allow_session_prompt: bool,
    #[serde(default)]
    pub access: AgentAccessDraft,
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
pub struct AgentPromptProfileDraft {
    pub id: String,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    /// Write-only in the public contract.
    pub system_prompt: String,
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
    pub system_prompt_sha256: String,
    #[serde(default)]
    pub tools: Vec<RedactedAgentTool>,
    #[serde(default)]
    pub memory_store_types: Vec<String>,
    #[serde(default)]
    pub ui_commands: Vec<RedactedAgentUiCommand>,
    pub behavior: AgentBehaviorDraft,
    pub agent_workspace_configured: bool,
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
pub struct RedactedAgentPromptProfile {
    pub id: String,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
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
mod tests {
    use super::*;

    #[test]
    fn list_request_keeps_a_stable_default_page_size() {
        let request: AgentAdminRequest = serde_json::from_value(serde_json::json!({
            "operation": "list_revisions",
            "agent_id": "oraculo"
        }))
        .unwrap();
        assert_eq!(
            request,
            AgentAdminRequest::ListRevisions {
                agent_id: AgentId::new("oraculo"),
                before_revision: None,
                limit: 50,
            }
        );
    }

    #[test]
    fn update_round_trip_carries_concurrency_and_secret_references_only() {
        let mut environment = BTreeMap::new();
        environment.insert(
            "TOKEN".into(),
            AgentSecretReference::Environment {
                name: "MCP_TOKEN".into(),
            },
        );
        let request = AgentAdminRequest::UpdateDefinition {
            expected_active_revision: 4,
            definition: Box::new(AgentDefinitionDraft {
                agent_id: AgentId::new("oraculo"),
                revision: 5,
                name: "Oraculo".into(),
                description: "companion".into(),
                provider_id: "anthropic".into(),
                default_model_id: "sonnet".into(),
                temperature: None,
                max_tokens: Some(32_000),
                system_prompt: "private prompt".into(),
                tools: vec![AgentToolDraft::McpServer {
                    name: "search".into(),
                    command: "mcp-search".into(),
                    args: vec!["serve".into()],
                    environment,
                }],
                memory_stores: Vec::new(),
                ui_commands: Vec::new(),
                behavior: AgentBehaviorDraft::default(),
                agent_workspace: None,
                prompt_profiles: Vec::new(),
                default_prompt_profile: None,
                allow_session_prompt: false,
                access: AgentAccessDraft::default(),
            }),
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["expected_active_revision"], 4);
        assert_eq!(
            json["definition"]["tools"][0]["environment"]["TOKEN"]["name"],
            "MCP_TOKEN"
        );
        assert_eq!(
            serde_json::from_value::<AgentAdminRequest>(json).unwrap(),
            request
        );
    }

    #[test]
    fn inspection_response_has_no_sensitive_definition_fields() {
        let response = AgentAdminResponse::Success {
            result: Box::new(AgentAdminResult::RevisionInspected {
                revision: AgentRevisionView {
                    definition: RedactedAgentDefinition {
                        agent_id: AgentId::new("oraculo"),
                        revision: 5,
                        name: "Oraculo".into(),
                        description: "companion".into(),
                        provider_id: "anthropic".into(),
                        default_model_id: "sonnet".into(),
                        system_prompt_sha256: "abc".into(),
                        tools: vec![RedactedAgentTool::McpServer {
                            name: "search".into(),
                        }],
                        memory_store_types: vec!["sqlite".into()],
                        ui_commands: Vec::new(),
                        behavior: AgentBehaviorDraft::default(),
                        agent_workspace_configured: true,
                        prompt_profiles: Vec::new(),
                        default_prompt_profile: None,
                        allow_session_prompt: false,
                        access: RedactedAgentAccess {
                            allow_authenticated: false,
                            allowed_principal_count: 1,
                            allowed_roles: vec!["operator".into()],
                        },
                    },
                    digest_sha256: "def".into(),
                    created_at_unix_secs: 1,
                    active: true,
                },
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        for forbidden in [
            "system_prompt\"",
            "command\"",
            "args\"",
            "environment\"",
            "workspace\"",
            "allowed_principals",
            "secret",
        ] {
            assert!(!json.contains(forbidden), "inspection leaked {forbidden}");
        }
    }

    #[test]
    fn conflict_error_preserves_machine_readable_revisions() {
        let response: AgentAdminResponse = serde_json::from_value(serde_json::json!({
            "status": "error",
            "error": {
                "code": "revision_conflict",
                "message": "active revision changed",
                "agent_id": "oraculo",
                "expected_active_revision": 4,
                "actual_active_revision": 5
            }
        }))
        .unwrap();
        assert!(matches!(
            response,
            AgentAdminResponse::Error {
                error: AgentAdminError {
                    code: AgentAdminErrorCode::RevisionConflict,
                    expected_active_revision: Some(4),
                    actual_active_revision: Some(5),
                    ..
                }
            }
        ));
    }
}
