//! Privileged, transport-neutral Agent definition administration.
//!
//! This module owns the boundary between public administration DTOs and the
//! runtime's internal configuration. Activation is deliberately returned as
//! a plan: the runtime must compose the requested revision successfully
//! before changing the registry head.

use std::collections::HashSet;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use sylvander_agent::spec::{
    AgentSpec, BehaviorConfig, McpServerConfig, MemoryStoreConfig, ModelConfig, PersonaConfig,
    ToolPresentationConfig, ToolRef, UiCommandConfig,
};
use sylvander_agent::tool::ToolHookConfig;
use sylvander_protocol::{
    AgentAdminError, AgentAdminErrorCode, AgentAdminRequest, AgentAdminResponse, AgentAdminResult,
    AgentBehaviorDraft, AgentDefinitionDraft, AgentRevisionView, AgentSecretReference,
    AgentToolDraft, AgentToolPresentationDraft, AuthenticatedPrincipal, PrincipalKind,
    RedactedAgentAccess, RedactedAgentDefinition, RedactedAgentHook, RedactedAgentPromptProfile,
    RedactedAgentTool, RedactedAgentUiCommand,
};

use crate::agent_registry::{AgentRegistry, AgentRegistryError, AgentRevision};
use crate::config::{
    AgentAccessConfig, AgentDefinitionConfig, PromptProfileConfig, ServerConfig,
    WorkspaceBindingConfig,
};
use sylvander_agent::prompt::{
    MAX_PROMPT_PROFILES, validate_identity, validate_profile_count, validate_profile_selectors,
    validate_prompt, validate_unique_identities,
};

pub(crate) const MAX_REVISION_PAGE_SIZE: u16 = 100;
const SECRET_REF_PREFIX: &str = "sylvander-secret-ref:v1:";

/// A request that must be atomically composed and activated by the runtime.
#[derive(Debug, Clone)]
pub(crate) enum AgentAdminDispatch {
    Response(AgentAdminResponse),
    Update {
        expected_active_revision: u64,
        definition: Box<AgentDefinitionConfig>,
    },
    Activate {
        agent_id: sylvander_protocol::AgentId,
        revision: u64,
        expected_active_revision: u64,
    },
    Rollback {
        agent_id: sylvander_protocol::AgentId,
        target_revision: u64,
        expected_active_revision: u64,
    },
}

/// Implements read and staged-update operations without mutating active state.
pub(crate) struct AgentAdminService<'a> {
    registry: &'a AgentRegistry,
    catalog: &'a ServerConfig,
}

impl<'a> AgentAdminService<'a> {
    #[must_use]
    pub(crate) const fn new(registry: &'a AgentRegistry, catalog: &'a ServerConfig) -> Self {
        Self { registry, catalog }
    }

    pub(crate) async fn dispatch(
        &self,
        principal: Option<&AuthenticatedPrincipal>,
        request: AgentAdminRequest,
    ) -> AgentAdminDispatch {
        if !is_agent_administrator(principal) {
            return AgentAdminDispatch::Response(error_response(AgentAdminError {
                code: AgentAdminErrorCode::Unauthorized,
                message: "Agent administration requires an administrator".into(),
                agent_id: None,
                revision: None,
                expected_active_revision: None,
                actual_active_revision: None,
            }));
        }
        match request {
            AgentAdminRequest::InspectRevision { agent_id, revision } => {
                let response = match self.registry.load(&agent_id, revision).await {
                    Ok(Some(stored)) => success(AgentAdminResult::RevisionInspected {
                        revision: redact_revision(&stored),
                    }),
                    Ok(None) => error_response(unknown_revision(agent_id, revision)),
                    Err(error) => error_response(map_registry_error(error)),
                };
                AgentAdminDispatch::Response(response)
            }
            AgentAdminRequest::ListRevisions {
                agent_id,
                before_revision,
                limit,
            } => AgentAdminDispatch::Response(
                self.list_revisions(agent_id, before_revision, limit).await,
            ),
            AgentAdminRequest::UpdateDefinition {
                expected_active_revision,
                definition,
            } => {
                match definition_from_draft(*definition).and_then(|definition| {
                    validate_against_catalog(self.catalog, &definition)?;
                    Ok(definition)
                }) {
                    Ok(definition) => AgentAdminDispatch::Update {
                        expected_active_revision,
                        definition: Box::new(definition),
                    },
                    Err(error) => AgentAdminDispatch::Response(error_response(error)),
                }
            }
            AgentAdminRequest::ActivateRevision {
                agent_id,
                revision,
                expected_active_revision,
            } => AgentAdminDispatch::Activate {
                agent_id,
                revision,
                expected_active_revision,
            },
            AgentAdminRequest::RollbackRevision {
                agent_id,
                target_revision,
                expected_active_revision,
            } => AgentAdminDispatch::Rollback {
                agent_id,
                target_revision,
                expected_active_revision,
            },
        }
    }

    async fn list_revisions(
        &self,
        agent_id: sylvander_protocol::AgentId,
        before: Option<u64>,
        limit: u16,
    ) -> AgentAdminResponse {
        if limit == 0 || limit > MAX_REVISION_PAGE_SIZE {
            return error_response(invalid_definition(format!(
                "revision page limit must be between 1 and {MAX_REVISION_PAGE_SIZE}"
            )));
        }
        match self.registry.inspect(&agent_id).await {
            Ok(stored) if stored.is_empty() => error_response(AgentAdminError {
                code: AgentAdminErrorCode::UnknownAgent,
                message: format!("unknown Agent `{agent_id}`"),
                agent_id: Some(agent_id),
                revision: None,
                expected_active_revision: None,
                actual_active_revision: None,
            }),
            Ok(stored) => {
                let active_revision = stored
                    .iter()
                    .find(|revision| revision.active)
                    .map_or(0, |revision| revision.definition.revision);
                let mut eligible = stored.iter().filter(|revision| {
                    before.is_none_or(|value| revision.definition.revision < value)
                });
                let revisions = eligible
                    .by_ref()
                    .take(usize::from(limit))
                    .map(redact_revision)
                    .collect::<Vec<_>>();
                let next_before_revision = eligible
                    .next()
                    .and_then(|_| revisions.last().map(|item| item.definition.revision));
                success(AgentAdminResult::RevisionsListed {
                    agent_id,
                    active_revision,
                    revisions,
                    next_before_revision,
                })
            }
            Err(error) => error_response(map_registry_error(error)),
        }
    }
}

#[must_use]
pub(crate) fn is_agent_administrator(principal: Option<&AuthenticatedPrincipal>) -> bool {
    principal.is_some_and(|principal| {
        principal.kind == PrincipalKind::System || principal.has_role("admin")
    })
}

pub(crate) fn definition_from_draft(
    draft: AgentDefinitionDraft,
) -> Result<AgentDefinitionConfig, AgentAdminError> {
    validate_draft(&draft)?;
    let mut tools = Vec::with_capacity(draft.tools.len());
    for tool in draft.tools {
        tools.push(match tool {
            AgentToolDraft::Builtin { name } => ToolRef::Builtin { name },
            AgentToolDraft::McpServer {
                name,
                command,
                args,
                environment,
            } => ToolRef::McpServer(McpServerConfig {
                name,
                command,
                args,
                envs: environment
                    .into_iter()
                    .map(|(name, reference)| encode_secret_reference(name, reference))
                    .collect::<Result<_, _>>()?,
            }),
        });
    }
    Ok(AgentDefinitionConfig {
        revision: draft.revision,
        spec: AgentSpec {
            id: draft.agent_id,
            name: draft.name,
            persona: PersonaConfig {
                system_prompt: draft.system_prompt,
                description: draft.description,
            },
            model: ModelConfig {
                provider: draft.provider_id,
                model_name: draft.default_model_id,
                allowed_models: draft.allowed_models,
                temperature: draft.temperature,
                max_tokens: draft.max_tokens,
            },
            tools,
            memory_stores: draft
                .memory_stores
                .into_iter()
                .map(|store| MemoryStoreConfig {
                    store_type: store.store_type,
                    path: PathBuf::from(store.path),
                })
                .collect(),
            ui_commands: draft
                .ui_commands
                .into_iter()
                .map(|command| UiCommandConfig {
                    id: command.id,
                    name: command.name,
                    usage: command.usage,
                    description: command.description,
                    hint: command.hint,
                    prompt: command.prompt,
                })
                .collect(),
            hooks: draft
                .hooks
                .into_iter()
                .map(|hook| ToolHookConfig {
                    name: hook.name,
                    phase: hook.phase,
                    command: hook.command,
                    timeout_secs: hook.timeout_secs,
                    blocking: hook.blocking,
                })
                .collect(),
            tool_presentations: draft
                .tool_presentations
                .into_iter()
                .map(|presentation| ToolPresentationConfig {
                    tool_name: presentation.tool_name,
                    label: presentation.label,
                    kind: presentation.kind,
                    target_field: presentation.target_field,
                })
                .collect(),
            behavior: BehaviorConfig {
                max_iterations: draft.behavior.max_iterations,
                max_retries: draft.behavior.max_retries,
            },
        },
        agent_workspace: draft
            .agent_workspace
            .map(|workspace| WorkspaceBindingConfig {
                execution_target: workspace.execution_target,
                path: workspace
                    .path
                    .into_os_string()
                    .into_string()
                    .expect("non-UTF-8 workspace paths are rejected during validation"),
                read_only: workspace.read_only,
                instruction_focus: workspace
                    .instruction_focus
                    .map(|focus| focus.to_string_lossy().into_owned()),
            }),
        workspace_mounts: draft
            .workspace_mounts
            .into_iter()
            .map(|mount| crate::config::WorkspaceMountConfig {
                reference: mount.reference,
                role: mount.role,
                binding: WorkspaceBindingConfig {
                    execution_target: mount.binding.execution_target,
                    path: mount
                        .binding
                        .path
                        .into_os_string()
                        .into_string()
                        .expect("non-UTF-8 workspace paths are rejected during validation"),
                    read_only: mount.binding.read_only,
                    instruction_focus: mount
                        .binding
                        .instruction_focus
                        .map(|focus| focus.to_string_lossy().into_owned()),
                },
                capabilities: mount.capabilities,
            })
            .collect(),
        prompt_profiles: draft
            .prompt_profiles
            .into_iter()
            .map(|profile| PromptProfileConfig {
                id: profile.id,
                qualified_models: profile.qualified_models,
                system_prompt: profile.system_prompt,
            })
            .collect(),
        default_prompt_profile: draft.default_prompt_profile,
        allow_session_prompt: draft.allow_session_prompt,
        access: AgentAccessConfig {
            allow_authenticated: draft.access.allow_authenticated,
            allowed_principals: draft.access.allowed_principals,
            allowed_roles: draft.access.allowed_roles,
        },
    })
}

#[must_use]
pub(crate) fn redact_revision(revision: &AgentRevision) -> AgentRevisionView {
    let definition = &revision.definition;
    let tools = definition
        .spec
        .tools
        .iter()
        .map(redact_tool)
        .collect::<Vec<_>>();
    AgentRevisionView {
        definition: RedactedAgentDefinition {
            agent_id: definition.spec.id.clone(),
            revision: definition.revision,
            name: definition.spec.name.clone(),
            description: definition.spec.persona.description.clone(),
            provider_id: definition.spec.model.provider.clone(),
            default_model_id: definition.spec.model.model_name.clone(),
            allowed_models: definition.spec.model.allowed_models.clone(),
            system_prompt_sha256: digest(&definition.spec.persona.system_prompt),
            tools,
            memory_store_types: definition
                .spec
                .memory_stores
                .iter()
                .map(|store| store.store_type.clone())
                .collect(),
            ui_commands: definition
                .spec
                .ui_commands
                .iter()
                .map(redact_command)
                .collect(),
            hooks: definition
                .spec
                .hooks
                .iter()
                .map(|hook| RedactedAgentHook {
                    name: hook.name.clone(),
                    phase: hook.phase,
                    timeout_secs: hook.timeout_secs,
                    blocking: hook.blocking,
                })
                .collect(),
            tool_presentations: definition
                .spec
                .tool_presentations
                .iter()
                .map(|presentation| AgentToolPresentationDraft {
                    tool_name: presentation.tool_name.clone(),
                    label: presentation.label.clone(),
                    kind: presentation.kind,
                    target_field: presentation.target_field.clone(),
                })
                .collect(),
            behavior: AgentBehaviorDraft {
                max_iterations: definition.spec.behavior.max_iterations,
                max_retries: definition.spec.behavior.max_retries,
            },
            agent_workspace_configured: definition.agent_workspace.is_some(),
            workspace_mount_count: definition.workspace_mounts.len(),
            prompt_profiles: definition
                .prompt_profiles
                .iter()
                .map(|profile| RedactedAgentPromptProfile {
                    id: profile.id.clone(),
                    qualified_models: profile.qualified_models.clone(),
                    system_prompt_sha256: digest(&profile.system_prompt),
                })
                .collect(),
            default_prompt_profile: definition.default_prompt_profile.clone(),
            allow_session_prompt: definition.allow_session_prompt,
            access: RedactedAgentAccess {
                allow_authenticated: definition.access.allow_authenticated,
                allowed_principal_count: definition.access.allowed_principals.len() as u32,
                allowed_roles: definition.access.allowed_roles.clone(),
            },
        },
        digest_sha256: revision.digest.clone(),
        created_at_unix_secs: revision.created_at,
        active: revision.active,
    }
}

#[must_use]
pub(crate) fn map_registry_error(error: AgentRegistryError) -> AgentAdminError {
    match error {
        AgentRegistryError::Invalid(_) => invalid_definition("Agent definition is invalid"),
        AgentRegistryError::UnknownAgent(agent_id) => AgentAdminError {
            code: AgentAdminErrorCode::UnknownAgent,
            message: format!("unknown Agent `{agent_id}`"),
            agent_id: Some(agent_id.into()),
            revision: None,
            expected_active_revision: None,
            actual_active_revision: None,
        },
        AgentRegistryError::UnknownRevision { agent_id, revision } => {
            unknown_revision(agent_id.into(), revision)
        }
        AgentRegistryError::Conflict {
            agent_id,
            expected,
            actual,
        } => AgentAdminError {
            code: AgentAdminErrorCode::RevisionConflict,
            message: "Agent active revision changed".into(),
            agent_id: Some(agent_id.into()),
            revision: None,
            expected_active_revision: Some(expected),
            actual_active_revision: Some(actual),
        },
        AgentRegistryError::NonSequential {
            agent_id,
            expected,
            actual,
        } => AgentAdminError {
            code: AgentAdminErrorCode::NonSequentialRevision,
            message: format!("next Agent revision must be {expected}"),
            agent_id: Some(agent_id.into()),
            revision: Some(actual),
            expected_active_revision: None,
            actual_active_revision: None,
        },
        AgentRegistryError::RevisionCollision { agent_id, revision } => AgentAdminError {
            code: AgentAdminErrorCode::RevisionCollision,
            message: "Agent revision already has different content".into(),
            agent_id: Some(agent_id.into()),
            revision: Some(revision),
            expected_active_revision: None,
            actual_active_revision: None,
        },
        AgentRegistryError::InvalidRollback { target, actual } => AgentAdminError {
            code: AgentAdminErrorCode::InvalidRollback,
            message: "rollback target must be older than the active revision".into(),
            agent_id: None,
            revision: Some(target),
            expected_active_revision: None,
            actual_active_revision: Some(actual),
        },
        AgentRegistryError::Storage(_) | AgentRegistryError::Task(_) => AgentAdminError {
            code: AgentAdminErrorCode::StorageUnavailable,
            message: "Agent registry is unavailable".into(),
            agent_id: None,
            revision: None,
            expected_active_revision: None,
            actual_active_revision: None,
        },
        AgentRegistryError::Serialization(_) | AgentRegistryError::Integrity(_) => {
            AgentAdminError {
                code: AgentAdminErrorCode::Internal,
                message: "Agent registry integrity check failed".into(),
                agent_id: None,
                revision: None,
                expected_active_revision: None,
                actual_active_revision: None,
            }
        }
    }
}

fn validate_against_catalog(
    catalog: &ServerConfig,
    definition: &AgentDefinitionConfig,
) -> Result<(), AgentAdminError> {
    catalog
        .validate_agent_shape_and_environment(definition)
        .map_err(|_| invalid_definition("Agent definition does not match the runtime catalog"))
}

fn validate_draft(draft: &AgentDefinitionDraft) -> Result<(), AgentAdminError> {
    let required = [
        ("agent_id", draft.agent_id.0.as_str()),
        ("name", draft.name.as_str()),
        ("provider_id", draft.provider_id.as_str()),
        ("default_model_id", draft.default_model_id.as_str()),
    ];
    if let Some((field, _)) = required.iter().find(|(_, value)| value.trim().is_empty()) {
        return Err(invalid_definition(format!("{field} must not be empty")));
    }
    if draft.revision == 0 || draft.behavior.max_iterations == 0 {
        return Err(invalid_definition(
            "revision and max_iterations must be positive",
        ));
    }
    if draft.temperature.is_some_and(|value| !value.is_finite()) || draft.max_tokens == Some(0) {
        return Err(invalid_definition("model tuning values are invalid"));
    }
    if draft.allowed_models.is_empty() {
        return Err(invalid_definition("allowed models must not be empty"));
    }
    let mut seen = HashSet::new();
    for model in &draft.allowed_models {
        let provider = model.provider_id.trim();
        let model_id = model.model_id.trim();
        if provider.is_empty() || model_id.is_empty() || !seen.insert((provider, model_id)) {
            return Err(invalid_definition(
                "allowed models must contain unique qualified identities",
            ));
        }
    }
    if !draft.allowed_models.iter().any(|model| {
        model.provider_id.trim() == draft.provider_id.trim()
            && model.model_id.trim() == draft.default_model_id.trim()
    }) {
        return Err(invalid_definition(
            "allowed models must contain the Agent default model",
        ));
    }
    if draft
        .agent_workspace
        .as_ref()
        .is_some_and(|workspace| workspace.path.to_str().is_none())
    {
        return Err(invalid_definition("workspace path must be valid UTF-8"));
    }
    unique_non_empty(
        draft
            .workspace_mounts
            .iter()
            .map(|mount| mount.reference.as_str()),
        "workspace mount",
    )?;
    if draft.workspace_mounts.iter().any(|mount| {
        mount.binding.path.to_str().is_none()
            || (mount.binding.read_only && (mount.capabilities.write || mount.capabilities.command))
    }) {
        return Err(invalid_definition(
            "workspace mount path or capability policy is invalid",
        ));
    }
    validate_prompt_draft(draft)?;
    unique_non_empty(
        draft.ui_commands.iter().map(|item| item.id.as_str()),
        "UI command",
    )?;
    if draft.hooks.len() > 32 {
        return Err(invalid_definition("at most 32 hooks are allowed"));
    }
    unique_non_empty(draft.hooks.iter().map(|hook| hook.name.as_str()), "hook")?;
    if draft.hooks.iter().any(|hook| {
        hook.name.chars().count() > 128
            || hook.name.chars().any(char::is_control)
            || hook.command.trim().is_empty()
            || hook.command.len() > 4096
            || !(1..=300).contains(&hook.timeout_secs)
    }) {
        return Err(invalid_definition(
            "hook identity, command, or timeout is outside the supported bounds",
        ));
    }
    if draft.tool_presentations.len() > 128 {
        return Err(invalid_definition(
            "at most 128 tool presentations are allowed",
        ));
    }
    unique_non_empty(
        draft
            .tool_presentations
            .iter()
            .map(|presentation| presentation.tool_name.as_str()),
        "tool presentation",
    )?;
    if draft.tool_presentations.iter().any(|presentation| {
        presentation.label.trim().is_empty()
            || presentation.label.chars().count() > 80
            || presentation
                .target_field
                .as_ref()
                .is_some_and(|field| field.trim().is_empty() || field.len() > 128)
    }) {
        return Err(invalid_definition(
            "tool presentation metadata is outside the supported bounds",
        ));
    }
    if draft
        .default_prompt_profile
        .as_ref()
        .is_some_and(|default| {
            !draft
                .prompt_profiles
                .iter()
                .any(|profile| profile.id == *default)
        })
    {
        return Err(invalid_definition("default prompt profile does not exist"));
    }
    for tool in &draft.tools {
        match tool {
            AgentToolDraft::Builtin { name } if name.trim().is_empty() => {
                return Err(invalid_definition("built-in tool name must not be empty"));
            }
            AgentToolDraft::McpServer {
                name,
                command,
                environment,
                ..
            } => {
                if name.trim().is_empty() || command.trim().is_empty() {
                    return Err(invalid_definition("MCP name and command must not be empty"));
                }
                for (name, reference) in environment {
                    if name.trim().is_empty() || !valid_secret_reference(reference) {
                        return Err(invalid_definition("MCP secret reference is invalid"));
                    }
                }
            }
            AgentToolDraft::Builtin { .. } => {}
        }
    }
    Ok(())
}

fn validate_prompt_draft(draft: &AgentDefinitionDraft) -> Result<(), AgentAdminError> {
    let result = validate_profile_count(draft.prompt_profiles.len())
        .and_then(|()| validate_prompt(&draft.system_prompt))
        .and_then(|()| {
            validate_unique_identities(
                draft
                    .prompt_profiles
                    .iter()
                    .map(|profile| profile.id.as_str()),
                MAX_PROMPT_PROFILES,
            )
        })
        .and_then(|()| match draft.default_prompt_profile.as_deref() {
            Some(default) => validate_identity(default),
            None => Ok(()),
        })
        .and_then(|()| {
            for profile in &draft.prompt_profiles {
                validate_prompt(&profile.system_prompt)?;
                validate_profile_selectors(&profile.qualified_models)?;
            }
            Ok(())
        });
    result.map_err(|issue| invalid_definition(format!("prompt configuration is invalid: {issue}")))
}

fn unique_non_empty<'a>(
    values: impl Iterator<Item = &'a str>,
    label: &str,
) -> Result<(), AgentAdminError> {
    let mut seen = HashSet::new();
    for value in values {
        if value.trim().is_empty() || !seen.insert(value) {
            return Err(invalid_definition(format!(
                "{label} ids must be non-empty and unique"
            )));
        }
    }
    Ok(())
}

fn valid_secret_reference(reference: &AgentSecretReference) -> bool {
    match reference {
        AgentSecretReference::Environment { name } => !name.trim().is_empty(),
        AgentSecretReference::File { path } => !path.trim().is_empty(),
    }
}

fn encode_secret_reference(
    name: String,
    reference: AgentSecretReference,
) -> Result<(String, String), AgentAdminError> {
    serde_json::to_string(&reference)
        .map(|encoded| (name, format!("{SECRET_REF_PREFIX}{encoded}")))
        .map_err(|_| invalid_definition("MCP secret reference cannot be encoded"))
}

fn redact_tool(tool: &ToolRef) -> RedactedAgentTool {
    match tool {
        ToolRef::Builtin { name } => RedactedAgentTool::Builtin { name: name.clone() },
        ToolRef::McpServer(server) => RedactedAgentTool::McpServer {
            name: server.name.clone(),
        },
    }
}

fn redact_command(command: &UiCommandConfig) -> RedactedAgentUiCommand {
    RedactedAgentUiCommand {
        id: command.id.clone(),
        name: command.name.clone(),
        usage: command.usage.clone(),
        description: command.description.clone(),
        hint: command.hint.clone(),
    }
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn success(result: AgentAdminResult) -> AgentAdminResponse {
    AgentAdminResponse::Success {
        result: Box::new(result),
    }
}

fn error_response(error: AgentAdminError) -> AgentAdminResponse {
    AgentAdminResponse::Error { error }
}

fn invalid_definition(message: impl Into<String>) -> AgentAdminError {
    AgentAdminError {
        code: AgentAdminErrorCode::InvalidDefinition,
        message: message.into(),
        agent_id: None,
        revision: None,
        expected_active_revision: None,
        actual_active_revision: None,
    }
}

fn unknown_revision(agent_id: sylvander_protocol::AgentId, revision: u64) -> AgentAdminError {
    AgentAdminError {
        code: AgentAdminErrorCode::UnknownRevision,
        message: format!("unknown Agent revision `{agent_id}`@{revision}"),
        agent_id: Some(agent_id),
        revision: Some(revision),
        expected_active_revision: None,
        actual_active_revision: None,
    }
}

#[cfg(test)]
#[path = "../tests/unit/agent_admin.rs"]
pub(crate) mod tests;
