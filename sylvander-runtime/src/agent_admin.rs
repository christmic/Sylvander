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
    ToolRef, UiCommandConfig,
};
use sylvander_protocol::{
    AgentAdminError, AgentAdminErrorCode, AgentAdminRequest, AgentAdminResponse, AgentAdminResult,
    AgentBehaviorDraft, AgentDefinitionDraft, AgentRevisionView, AgentSecretReference,
    AgentToolDraft, AuthenticatedPrincipal, PrincipalKind, RedactedAgentAccess,
    RedactedAgentDefinition, RedactedAgentPromptProfile, RedactedAgentTool, RedactedAgentUiCommand,
};
#[cfg(test)]
use sylvander_protocol::{AgentPromptProfileDraft, AgentUiCommandDraft, SessionWorkspaceBinding};

use crate::agent_registry::{AgentRegistry, AgentRegistryError, AgentRevision};
use crate::config::{
    AgentAccessConfig, AgentDefinitionConfig, PromptProfileConfig, ServerConfig,
    WorkspaceBindingConfig,
};
use crate::prompt_limits::{
    MAX_PROMPT_PROFILES, MAX_PROMPT_SELECTORS_PER_KIND, validate_identity, validate_profile_count,
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
            mcp_servers: Vec::new(),
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
            }),
        prompt_profiles: draft
            .prompt_profiles
            .into_iter()
            .map(|profile| PromptProfileConfig {
                id: profile.id,
                providers: profile.providers,
                models: profile.models,
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

#[cfg(test)]
pub(crate) fn draft_from_definition(
    definition: &AgentDefinitionConfig,
) -> Result<AgentDefinitionDraft, AgentAdminError> {
    let mut tools = definition
        .spec
        .tools
        .iter()
        .map(tool_to_draft)
        .collect::<Result<Vec<_>, _>>()?;
    tools.extend(
        definition
            .spec
            .mcp_servers
            .iter()
            .map(mcp_to_draft)
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(AgentDefinitionDraft {
        agent_id: definition.spec.id.clone(),
        revision: definition.revision,
        name: definition.spec.name.clone(),
        description: definition.spec.persona.description.clone(),
        provider_id: definition.spec.model.provider.clone(),
        default_model_id: definition.spec.model.model_name.clone(),
        allowed_models: definition.spec.model.allowed_models.clone(),
        temperature: definition.spec.model.temperature,
        max_tokens: definition.spec.model.max_tokens,
        system_prompt: definition.spec.persona.system_prompt.clone(),
        tools,
        memory_stores: definition
            .spec
            .memory_stores
            .iter()
            .map(|store| {
                Ok(sylvander_protocol::AgentMemoryStoreDraft {
                    store_type: store.store_type.clone(),
                    path: store
                        .path
                        .to_str()
                        .ok_or_else(|| invalid_definition("memory store path must be valid UTF-8"))?
                        .to_owned(),
                })
            })
            .collect::<Result<_, AgentAdminError>>()?,
        ui_commands: definition
            .spec
            .ui_commands
            .iter()
            .map(command_to_draft)
            .collect(),
        behavior: AgentBehaviorDraft {
            max_iterations: definition.spec.behavior.max_iterations,
            max_retries: definition.spec.behavior.max_retries,
        },
        agent_workspace: definition.agent_workspace.as_ref().map(|workspace| {
            SessionWorkspaceBinding {
                execution_target: workspace.execution_target.clone(),
                path: PathBuf::from(&workspace.path),
                read_only: workspace.read_only,
            }
        }),
        prompt_profiles: definition
            .prompt_profiles
            .iter()
            .map(profile_to_draft)
            .collect(),
        default_prompt_profile: definition.default_prompt_profile.clone(),
        allow_session_prompt: definition.allow_session_prompt,
        access: sylvander_protocol::AgentAccessDraft {
            allow_authenticated: definition.access.allow_authenticated,
            allowed_principals: definition.access.allowed_principals.clone(),
            allowed_roles: definition.access.allowed_roles.clone(),
        },
    })
}

#[must_use]
pub(crate) fn redact_revision(revision: &AgentRevision) -> AgentRevisionView {
    let definition = &revision.definition;
    let mut tools = definition
        .spec
        .tools
        .iter()
        .map(redact_tool)
        .collect::<Vec<_>>();
    tools.extend(
        definition
            .spec
            .mcp_servers
            .iter()
            .map(|server| RedactedAgentTool::McpServer {
                name: server.name.clone(),
            }),
    );
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
            behavior: AgentBehaviorDraft {
                max_iterations: definition.spec.behavior.max_iterations,
                max_retries: definition.spec.behavior.max_retries,
            },
            agent_workspace_configured: definition.agent_workspace.is_some(),
            prompt_profiles: definition
                .prompt_profiles
                .iter()
                .map(|profile| RedactedAgentPromptProfile {
                    id: profile.id.clone(),
                    providers: profile.providers.clone(),
                    models: profile.models.clone(),
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
    validate_prompt_draft(draft)?;
    unique_non_empty(
        draft.ui_commands.iter().map(|item| item.id.as_str()),
        "UI command",
    )?;
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
                validate_unique_identities(
                    profile.providers.iter().map(String::as_str),
                    MAX_PROMPT_SELECTORS_PER_KIND,
                )?;
                validate_unique_identities(
                    profile.models.iter().map(String::as_str),
                    MAX_PROMPT_SELECTORS_PER_KIND,
                )?;
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

#[cfg(test)]
fn decode_secret_reference(value: &str) -> Result<AgentSecretReference, AgentAdminError> {
    let encoded = value
        .strip_prefix(SECRET_REF_PREFIX)
        .ok_or_else(|| invalid_definition("legacy MCP environment values cannot be exported"))?;
    serde_json::from_str(encoded)
        .map_err(|_| invalid_definition("stored MCP secret reference is invalid"))
}

#[cfg(test)]
fn tool_to_draft(tool: &ToolRef) -> Result<AgentToolDraft, AgentAdminError> {
    match tool {
        ToolRef::Builtin { name } => Ok(AgentToolDraft::Builtin { name: name.clone() }),
        ToolRef::McpServer(server) => mcp_to_draft(server),
    }
}

#[cfg(test)]
fn mcp_to_draft(server: &McpServerConfig) -> Result<AgentToolDraft, AgentAdminError> {
    let environment = server
        .envs
        .iter()
        .map(|(name, value)| Ok((name.clone(), decode_secret_reference(value)?)))
        .collect::<Result<_, AgentAdminError>>()?;
    Ok(AgentToolDraft::McpServer {
        name: server.name.clone(),
        command: server.command.clone(),
        args: server.args.clone(),
        environment,
    })
}

#[cfg(test)]
fn command_to_draft(command: &UiCommandConfig) -> AgentUiCommandDraft {
    AgentUiCommandDraft {
        id: command.id.clone(),
        name: command.name.clone(),
        usage: command.usage.clone(),
        description: command.description.clone(),
        hint: command.hint.clone(),
        prompt: command.prompt.clone(),
    }
}

#[cfg(test)]
fn profile_to_draft(profile: &PromptProfileConfig) -> AgentPromptProfileDraft {
    AgentPromptProfileDraft {
        id: profile.id.clone(),
        providers: profile.providers.clone(),
        models: profile.models.clone(),
        system_prompt: profile.system_prompt.clone(),
    }
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
mod tests {
    use super::*;
    use sylvander_protocol::{AgentAccessDraft, AgentId, AuthenticationMethod, ModelSelection};

    fn model(provider_id: &str, model_id: &str) -> ModelSelection {
        ModelSelection {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        }
    }

    fn catalog() -> ServerConfig {
        ServerConfig::from_toml(
            r#"
schema_version = 1
[[model_providers]]
id = "primary"
base_url = "https://primary.invalid"
[model_providers.api_key]
source = "env"
name = "PRIMARY_TOKEN"
[[model_providers.models]]
id = "sonnet"

[[model_providers]]
id = "secondary"
base_url = "https://secondary.invalid"
[model_providers.api_key]
source = "env"
name = "SECONDARY_TOKEN"
[[model_providers.models]]
id = "sonnet"
"#,
        )
        .unwrap()
    }

    fn draft() -> AgentDefinitionDraft {
        AgentDefinitionDraft {
            agent_id: AgentId::new("oraculo"),
            revision: 2,
            name: "Oraculo".into(),
            description: "companion".into(),
            provider_id: "primary".into(),
            default_model_id: "sonnet".into(),
            allowed_models: vec![model("primary", "sonnet")],
            temperature: Some(0.2),
            max_tokens: Some(1024),
            system_prompt: "never reveal me".into(),
            tools: vec![AgentToolDraft::McpServer {
                name: "search".into(),
                command: "mcp-search".into(),
                args: vec!["serve".into()],
                environment: [(
                    "TOKEN".into(),
                    AgentSecretReference::Environment {
                        name: "SEARCH_TOKEN".into(),
                    },
                )]
                .into_iter()
                .collect(),
            }],
            memory_stores: Vec::new(),
            ui_commands: Vec::new(),
            behavior: AgentBehaviorDraft::default(),
            agent_workspace: None,
            prompt_profiles: Vec::new(),
            default_prompt_profile: None,
            allow_session_prompt: false,
            access: AgentAccessDraft::default(),
        }
    }

    #[test]
    fn system_and_admin_role_are_privileged() {
        let mut user = AuthenticatedPrincipal::user("operator", AuthenticationMethod::Internal);
        assert!(!is_agent_administrator(Some(&user)));
        user.roles.push("admin".into());
        assert!(is_agent_administrator(Some(&user)));
        user.kind = PrincipalKind::System;
        user.roles.clear();
        assert!(is_agent_administrator(Some(&user)));
        assert!(!is_agent_administrator(None));
    }

    #[test]
    fn definition_conversion_preserves_only_secret_references() {
        let config = definition_from_draft(draft()).unwrap();
        let encoded = match &config.spec.tools[0] {
            ToolRef::McpServer(server) => &server.envs["TOKEN"],
            ToolRef::Builtin { .. } => panic!("expected MCP server"),
        };
        assert!(encoded.starts_with(SECRET_REF_PREFIX));
        assert!(!encoded.contains("secret-value"));
        assert_eq!(draft_from_definition(&config).unwrap(), draft());
    }

    #[test]
    fn legacy_inline_mcp_environment_value_fails_closed() {
        let mut config = definition_from_draft(draft()).unwrap();
        let ToolRef::McpServer(server) = &mut config.spec.tools[0] else {
            panic!("expected MCP server");
        };
        server.envs.insert("TOKEN".into(), "raw-secret".into());
        let error = draft_from_definition(&config).unwrap_err();
        assert_eq!(error.code, AgentAdminErrorCode::InvalidDefinition);
        assert!(!error.message.contains("raw-secret"));
    }

    #[test]
    fn redaction_returns_hashes_and_counts_not_sensitive_values() {
        let config = definition_from_draft(draft()).unwrap();
        let view = redact_revision(&AgentRevision {
            definition: config,
            digest: "definition-digest".into(),
            created_at: 7,
            active: false,
        });
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("never reveal me"));
        assert!(!json.contains("SEARCH_TOKEN"));
        assert!(!json.contains("mcp-search"));
        assert_eq!(
            view.definition.system_prompt_sha256,
            digest("never reveal me")
        );
        assert_eq!(view.definition.allowed_models, draft().allowed_models);
    }

    #[test]
    fn cross_provider_same_model_id_is_a_valid_exact_allowlist() {
        let mut candidate = draft();
        candidate.allowed_models.push(model("secondary", "sonnet"));
        let definition = definition_from_draft(candidate).unwrap();

        validate_against_catalog(&catalog(), &definition).unwrap();
        assert_eq!(definition.spec.model.allowed_models.len(), 2);
    }

    #[test]
    fn allowed_models_must_be_non_empty_unique_and_include_the_default() {
        for allowed_models in [
            Vec::new(),
            vec![model("primary", "sonnet"), model("primary", "sonnet")],
            vec![model("secondary", "sonnet")],
            vec![model("", "sonnet"), model("primary", "sonnet")],
        ] {
            let mut candidate = draft();
            candidate.allowed_models = allowed_models;
            assert_eq!(
                definition_from_draft(candidate).unwrap_err().code,
                AgentAdminErrorCode::InvalidDefinition
            );
        }
    }

    #[test]
    fn boot_catalog_rejects_unknown_allowed_provider_or_model() {
        for unknown in [model("missing", "sonnet"), model("secondary", "missing")] {
            let mut candidate = draft();
            candidate.allowed_models.push(unknown);
            let definition = definition_from_draft(candidate).unwrap();
            let mut boot_catalog = catalog();
            boot_catalog.agents.push(definition);
            assert!(boot_catalog.validate().is_err());
        }
    }

    #[test]
    fn update_rejects_unknown_execution_target() {
        let mut definition = definition_from_draft(draft()).unwrap();
        definition.agent_workspace = Some(WorkspaceBindingConfig {
            execution_target: "missing-target".into(),
            path: "/workspace".into(),
            read_only: false,
        });

        assert_eq!(
            validate_against_catalog(&catalog(), &definition)
                .unwrap_err()
                .code,
            AgentAdminErrorCode::InvalidDefinition
        );
    }

    #[test]
    fn registry_storage_errors_do_not_expose_internal_details() {
        let response = map_registry_error(AgentRegistryError::Storage(
            "/private/db?token=secret-value".into(),
        ));
        assert_eq!(response.code, AgentAdminErrorCode::StorageUnavailable);
        assert!(!response.message.contains("secret-value"));
    }

    #[tokio::test]
    async fn dynamic_qualified_update_is_a_plan_and_does_not_write_the_registry() {
        let catalog = ServerConfig::from_toml(
            r#"
schema_version = 1
[[model_providers]]
id = "primary"
base_url = "https://example.invalid"
[model_providers.api_key]
source = "env"
name = "TEST_TOKEN"
[[model_providers.models]]
id = "sonnet"
[[agents]]
revision = 1
[agents.spec]
id = "oraculo"
name = "Oraculo"
[agents.spec.model]
provider = "primary"
model_name = "sonnet"
"#,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::open(directory.path().join("agents.db"))
            .await
            .unwrap();
        let mut principal = AuthenticatedPrincipal::user("system", AuthenticationMethod::Internal);
        principal.kind = PrincipalKind::System;
        let mut candidate = draft();
        candidate.provider_id = "discovered-provider".into();
        candidate.default_model_id = "discovered-model".into();
        candidate.allowed_models = vec![model("discovered-provider", "discovered-model")];
        let dispatch = AgentAdminService::new(&registry, &catalog)
            .dispatch(
                Some(&principal),
                AgentAdminRequest::UpdateDefinition {
                    expected_active_revision: 1,
                    definition: Box::new(candidate),
                },
            )
            .await;
        assert!(matches!(
            dispatch,
            AgentAdminDispatch::Update {
                expected_active_revision: 1,
                ..
            }
        ));
        assert!(
            registry
                .inspect(&AgentId::new("oraculo"))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn invalid_prompt_update_is_content_free_and_never_reaches_storage() {
        let catalog = catalog();
        let directory = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::open(directory.path().join("agents.db"))
            .await
            .unwrap();
        let mut principal = AuthenticatedPrincipal::user("system", AuthenticationMethod::Internal);
        principal.kind = PrincipalKind::System;
        let mut candidate = draft();
        candidate.system_prompt = "private\0prompt".into();

        let dispatch = AgentAdminService::new(&registry, &catalog)
            .dispatch(
                Some(&principal),
                AgentAdminRequest::UpdateDefinition {
                    expected_active_revision: 1,
                    definition: Box::new(candidate),
                },
            )
            .await;
        let AgentAdminDispatch::Response(AgentAdminResponse::Error { error }) = dispatch else {
            panic!("invalid prompt must return a public error before staging");
        };
        assert_eq!(error.code, AgentAdminErrorCode::InvalidDefinition);
        assert!(error.message.contains("prompt configuration is invalid"));
        assert!(!error.message.contains("private"));
        assert!(
            registry
                .inspect(&AgentId::new("oraculo"))
                .await
                .unwrap()
                .is_empty()
        );
    }
}
