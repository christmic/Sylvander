//! Production composition of configured Agent runs.

use std::collections::HashSet;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use sylvander_agent::bus::MessageBus;
use sylvander_agent::run::AgentRun;
use sylvander_agent::session_store::SessionStore;
use sylvander_agent::spec::AgentSpec;
use sylvander_agent::tool::ToolRegistry;
use sylvander_agent::tools::memory::InMemoryMemoryStore;
use sylvander_agent::tools::{
    AskUserTool, EditTool, MemoryReadTool, PresentPlanTool, ReadTool, StartBackgroundTaskTool,
    UpdatePlanTool, WriteTool,
};
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use sylvander_protocol::{
    ApprovalPolicy, FileAccess, NetworkAccess, PermissionProfile, ReasoningEffort,
    SessionConfigOverrides, SessionConfigProvenance, SessionConfigSource, SessionConfigSourceKind,
    SessionEffectiveConfig, SessionWorkspaceBinding,
};

use crate::config::{
    AgentDefinitionConfig, ModelDefinitionConfig, ModelProviderConfig, SecretResolver, ServerConfig,
};

/// A configured run plus the metadata needed by protocol adapters.
#[derive(Clone)]
pub struct ConfiguredAgent {
    pub spec: AgentSpec,
    pub run: AgentRun,
    pub models: Vec<ModelInfo>,
    pub approval_enabled: bool,
    pub definition: AgentDefinitionConfig,
    pub execution_targets: HashSet<String>,
}

/// Build every configured Agent without starting background tasks.
pub fn build_agents(
    config: &ServerConfig,
    bus: Arc<dyn MessageBus>,
    sessions: Arc<dyn SessionStore>,
    secrets: &dyn SecretResolver,
) -> Result<Vec<ConfiguredAgent>, CompositionError> {
    config
        .agents
        .iter()
        .map(|agent| build_agent(config, agent, bus.clone(), sessions.clone(), secrets))
        .collect()
}

fn build_agent(
    config: &ServerConfig,
    definition: &AgentDefinitionConfig,
    bus: Arc<dyn MessageBus>,
    sessions: Arc<dyn SessionStore>,
    secrets: &dyn SecretResolver,
) -> Result<ConfiguredAgent, CompositionError> {
    let provider = config
        .model_providers
        .iter()
        .find(|provider| provider.id == definition.spec.model.provider)
        .ok_or_else(|| CompositionError::MissingProvider(definition.spec.model.provider.clone()))?;
    let api_key = secrets
        .resolve(&provider.api_key)
        .map_err(|error| CompositionError::Secret(provider.id.clone(), error.to_string()))?;
    let client =
        AnthropicClient::builder()
            .api_key(api_key.as_str().map_err(|error| {
                CompositionError::Secret(provider.id.clone(), error.to_string())
            })?)
            .base_url(&provider.base_url)
            .build()
            .map_err(|error| CompositionError::Client(provider.id.clone(), error.to_string()))?;

    let models = model_catalog(provider)?;
    let primary = models
        .iter()
        .find(|model| model.id == definition.spec.model.model_name)
        .ok_or_else(|| CompositionError::MissingModel {
            provider: provider.id.clone(),
            model: definition.spec.model.model_name.clone(),
        })?;
    let mut spec = definition.spec.clone();
    apply_default_prompt(definition, &mut spec)?;

    let memory = Arc::new(InMemoryMemoryStore::new());
    let tools = ToolRegistry::new()
        .register(ReadTool::new("/"))
        .register(WriteTool::new("/"))
        .register(EditTool::new("/"))
        .register(MemoryReadTool::new(memory.clone()))
        .register(AskUserTool::new())
        .register(PresentPlanTool::new())
        .register(UpdatePlanTool::new())
        .register(StartBackgroundTaskTool::new());

    let mut builder = AgentRun::builder(spec.clone(), client)
        .bus(bus)
        .session_store(sessions)
        .memory(memory)
        .override_tools(tools)
        .available_models(models.clone())
        .prompt_profiles(
            definition
                .prompt_profiles
                .iter()
                .map(|profile| (profile.id.clone(), profile.system_prompt.clone()))
                .collect(),
        )
        .model_capabilities(primary.capabilities);
    if let Some(path) = &config.server.workspace_journal {
        builder = builder.workspace_journal(path);
    }
    if config.server.approval.enabled {
        builder = builder.enable_approval();
    }
    if let Some(path) = &config.server.approval.persistent_store {
        builder = builder.approval_store(path);
    }
    let run = builder
        .build()
        .map_err(|error| CompositionError::Agent(spec.id.to_string(), error.to_string()))?;

    Ok(ConfiguredAgent {
        spec,
        run,
        models,
        approval_enabled: config.server.approval.enabled,
        definition: definition.clone(),
        execution_targets: config
            .execution_targets
            .iter()
            .map(|target| target.id.clone())
            .chain(std::iter::once("local".into()))
            .collect(),
    })
}

/// Resolve sparse session overrides against one immutable Agent definition.
pub fn resolve_session_config(
    agent: &ConfiguredAgent,
    overrides: &SessionConfigOverrides,
    channel_workspace: Option<(&str, &crate::config::WorkspaceBindingConfig)>,
    legacy_workspace: Option<&std::path::Path>,
) -> Result<SessionEffectiveConfig, CompositionError> {
    let definition = &agent.definition;
    let provider_id = definition.spec.model.provider.clone();
    let model_id = overrides
        .model_id
        .clone()
        .unwrap_or_else(|| definition.spec.model.model_name.clone());
    let model = agent
        .models
        .iter()
        .find(|candidate| candidate.id == model_id)
        .ok_or_else(|| CompositionError::MissingModel {
            provider: provider_id.clone(),
            model: model_id.clone(),
        })?;
    let reasoning_effort = overrides.reasoning_effort.unwrap_or_default();
    if reasoning_effort != ReasoningEffort::Off
        && !model
            .capabilities
            .contains(ModelCapabilities::EXTENDED_THINKING)
    {
        return Err(CompositionError::UnsupportedReasoning(model_id));
    }

    let permissions = overrides.permissions.clone().unwrap_or(PermissionProfile {
        file_access: FileAccess::WorkspaceWrite,
        network_access: NetworkAccess::Denied,
        approval_policy: if agent.approval_enabled {
            ApprovalPolicy::Ask
        } else {
            ApprovalPolicy::Allow
        },
    });
    if permissions.approval_policy == ApprovalPolicy::Ask && !agent.approval_enabled {
        return Err(CompositionError::ApprovalDisabled);
    }

    let prompt_profile = overrides
        .prompt_profile
        .clone()
        .or_else(|| definition.default_prompt_profile.clone());
    let mut system_prompt = definition.spec.persona.system_prompt.clone();
    if let Some(profile_id) = &prompt_profile {
        let profile = definition
            .prompt_profiles
            .iter()
            .find(|profile| &profile.id == profile_id)
            .ok_or_else(|| CompositionError::MissingPromptProfile {
                agent: definition.spec.id.to_string(),
                profile: profile_id.clone(),
            })?;
        if (!profile.providers.is_empty() && !profile.providers.contains(&provider_id))
            || (!profile.models.is_empty() && !profile.models.contains(&model_id))
        {
            return Err(CompositionError::IncompatiblePromptProfile {
                profile: profile_id.clone(),
                provider: provider_id.clone(),
                model: model_id.clone(),
            });
        }
        system_prompt.clone_from(&profile.system_prompt);
    }
    if let Some(prompt) = &overrides.system_prompt {
        if !definition.allow_session_prompt {
            return Err(CompositionError::SessionPromptDisabled);
        }
        system_prompt.clone_from(prompt);
    }

    let agent_workspace = definition.agent_workspace.as_ref().map(workspace_binding);
    let user_workspace = overrides
        .user_workspace
        .clone()
        .or_else(|| channel_workspace.map(|(_, workspace)| workspace_binding(workspace)))
        .or_else(|| {
            legacy_workspace.map(|path| SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: path.to_path_buf(),
                read_only: false,
            })
        });
    let execution_target = overrides
        .execution_target
        .clone()
        .or_else(|| {
            user_workspace
                .as_ref()
                .map(|workspace| workspace.execution_target.clone())
        })
        .or_else(|| {
            agent_workspace
                .as_ref()
                .map(|workspace| workspace.execution_target.clone())
        })
        .unwrap_or_else(|| "local".into());
    if !agent.execution_targets.contains(&execution_target) {
        return Err(CompositionError::MissingExecutionTarget(execution_target));
    }
    let agent_default = source(SessionConfigSourceKind::AgentDefault, &definition.spec.id.0);
    let session_override = source(SessionConfigSourceKind::SessionOverride, "session");
    let legacy = source(
        SessionConfigSourceKind::LegacyMigration,
        "metadata.workspace",
    );
    let channel_default = channel_workspace
        .map(|(channel, _)| source(SessionConfigSourceKind::ChannelDefault, channel));

    Ok(SessionEffectiveConfig {
        agent_id: definition.spec.id.clone(),
        agent_revision: definition.revision,
        provider_id,
        model_id,
        reasoning_effort,
        permissions,
        prompt_profile,
        system_prompt_sha256: format!("{:x}", Sha256::digest(system_prompt.as_bytes())),
        agent_workspace,
        user_workspace,
        execution_target,
        provenance: SessionConfigProvenance {
            model: choose(
                overrides.model_id.is_some(),
                &session_override,
                &agent_default,
            ),
            reasoning_effort: choose(
                overrides.reasoning_effort.is_some(),
                &session_override,
                &agent_default,
            ),
            permissions: choose(
                overrides.permissions.is_some(),
                &session_override,
                &agent_default,
            ),
            prompt_profile: choose(
                overrides.prompt_profile.is_some(),
                &session_override,
                &agent_default,
            ),
            system_prompt: choose(
                overrides.system_prompt.is_some(),
                &session_override,
                &agent_default,
            ),
            agent_workspace: agent_default.clone(),
            user_workspace: if overrides.user_workspace.is_some() {
                session_override.clone()
            } else if let Some(source) = &channel_default {
                source.clone()
            } else if legacy_workspace.is_some() {
                legacy.clone()
            } else {
                agent_default.clone()
            },
            execution_target: if overrides.execution_target.is_some() {
                session_override
            } else if overrides.user_workspace.is_some() {
                source(
                    SessionConfigSourceKind::SessionOverride,
                    "session.user_workspace",
                )
            } else if let Some(source) = channel_default {
                source
            } else if overrides.user_workspace.is_none() && legacy_workspace.is_some() {
                legacy
            } else {
                agent_default
            },
        },
    })
}

fn workspace_binding(workspace: &crate::config::WorkspaceBindingConfig) -> SessionWorkspaceBinding {
    SessionWorkspaceBinding {
        execution_target: workspace.execution_target.clone(),
        path: workspace.path.clone().into(),
        read_only: workspace.read_only,
    }
}

fn source(kind: SessionConfigSourceKind, reference: &str) -> SessionConfigSource {
    SessionConfigSource {
        kind,
        reference: Some(reference.into()),
    }
}

fn choose(
    overridden: bool,
    override_source: &SessionConfigSource,
    default_source: &SessionConfigSource,
) -> SessionConfigSource {
    if overridden {
        override_source.clone()
    } else {
        default_source.clone()
    }
}

fn apply_default_prompt(
    definition: &AgentDefinitionConfig,
    spec: &mut AgentSpec,
) -> Result<(), CompositionError> {
    let Some(profile_id) = &definition.default_prompt_profile else {
        return Ok(());
    };
    let profile = definition
        .prompt_profiles
        .iter()
        .find(|profile| &profile.id == profile_id)
        .ok_or_else(|| CompositionError::MissingPromptProfile {
            agent: spec.id.to_string(),
            profile: profile_id.clone(),
        })?;
    spec.persona
        .system_prompt
        .clone_from(&profile.system_prompt);
    Ok(())
}

fn model_catalog(provider: &ModelProviderConfig) -> Result<Vec<ModelInfo>, CompositionError> {
    provider
        .models
        .iter()
        .map(|model| {
            ModelInfo::builder()
                .id(&model.id)
                .context_window(model.context_window)
                .max_output_tokens(model.max_output_tokens)
                .capabilities(model_capabilities(model)?)
                .build()
                .ok_or_else(|| CompositionError::InvalidModel(model.id.clone()))
        })
        .collect()
}

fn model_capabilities(
    model: &ModelDefinitionConfig,
) -> Result<ModelCapabilities, CompositionError> {
    let mut result = ModelCapabilities::empty();
    for capability in &model.capabilities {
        result |= match capability.as_str() {
            "extended_thinking" | "reasoning" => ModelCapabilities::EXTENDED_THINKING,
            "prompt_caching" => ModelCapabilities::PROMPT_CACHING,
            "structured_output" => ModelCapabilities::STRUCTURED_OUTPUT,
            "tool_use" => ModelCapabilities::TOOL_USE,
            "vision" => ModelCapabilities::VISION,
            "document_input" => ModelCapabilities::DOCUMENT_INPUT,
            unknown => {
                return Err(CompositionError::UnknownCapability {
                    model: model.id.clone(),
                    capability: unknown.to_string(),
                });
            }
        };
    }
    Ok(result)
}

#[derive(Debug, thiserror::Error)]
pub enum CompositionError {
    #[error("model provider `{0}` is unavailable")]
    MissingProvider(String),
    #[error("model `{model}` is unavailable from provider `{provider}`")]
    MissingModel { provider: String, model: String },
    #[error("model `{0}` does not support reasoning")]
    UnsupportedReasoning(String),
    #[error("execution target `{0}` is unavailable")]
    MissingExecutionTarget(String),
    #[error("approval policy `ask` requires approvals to be enabled")]
    ApprovalDisabled,
    #[error("session system prompt overrides are disabled")]
    SessionPromptDisabled,
    #[error("prompt profile `{profile}` does not support {provider}/{model}")]
    IncompatiblePromptProfile {
        profile: String,
        provider: String,
        model: String,
    },
    #[error("failed to resolve secret for provider `{0}`: {1}")]
    Secret(String, String),
    #[error("failed to create client for provider `{0}`: {1}")]
    Client(String, String),
    #[error("model `{0}` has invalid metadata")]
    InvalidModel(String),
    #[error("model `{model}` has unknown capability `{capability}`")]
    UnknownCapability { model: String, capability: String },
    #[error("Agent `{agent}` has no prompt profile `{profile}`")]
    MissingPromptProfile { agent: String, profile: String },
    #[error("failed to build Agent `{0}`: {1}")]
    Agent(String, String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylvander_agent::bus::InProcessMessageBus;
    use sylvander_agent::session_store::SqliteSessionStore;

    #[tokio::test]
    async fn configured_agent_uses_catalog_prompt_and_secret_reference() {
        let directory = tempfile::TempDir::new().unwrap();
        let secret_path = directory.path().join("provider.key");
        std::fs::write(&secret_path, "test-secret\n").unwrap();
        let input = format!(
            r#"
schema_version = 1

[[model_providers]]
id = "primary"
base_url = "https://models.example.test"

[model_providers.api_key]
source = "file"
path = "{}"

[[model_providers.models]]
id = "model-a"
context_window = 100000
max_output_tokens = 16000
capabilities = ["tool_use", "vision"]

[[execution_targets]]
id = "local"

[execution_targets.transport]
kind = "local"

[[agents]]
default_prompt_profile = "optimized"
allow_session_prompt = false

[agents.spec]
id = "assistant"
name = "Sylvander"

[agents.spec.model]
provider = "primary"
model_name = "model-a"

[[agents.prompt_profiles]]
id = "optimized"
providers = ["primary"]
models = ["model-a"]
system_prompt = "Optimized system prompt"

[[channels]]
id = "terminal"
default_agent = "assistant"

[channels.transport]
kind = "unix"
path = "/tmp/sylvander-test.sock"
"#,
            secret_path.display()
        );
        let config = ServerConfig::from_toml(&input).unwrap();
        let bus: Arc<dyn MessageBus> = Arc::new(InProcessMessageBus::new());
        let sessions: Arc<dyn SessionStore> =
            Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());

        let agents =
            build_agents(&config, bus, sessions, &crate::config::SystemSecretResolver).unwrap();

        assert_eq!(agents.len(), 1);
        assert_eq!(
            agents[0].spec.persona.system_prompt,
            "Optimized system prompt"
        );
        assert!(
            agents[0].models[0]
                .capabilities
                .contains(ModelCapabilities::TOOL_USE | ModelCapabilities::VISION)
        );

        let effective = resolve_session_config(
            &agents[0],
            &SessionConfigOverrides::default(),
            None,
            Some(std::path::Path::new("/work/project")),
        )
        .unwrap();
        assert_eq!(effective.model_id, "model-a");
        assert_eq!(effective.prompt_profile.as_deref(), Some("optimized"));
        assert_eq!(effective.execution_target, "local");
        assert_eq!(
            effective.user_workspace.unwrap().path,
            std::path::PathBuf::from("/work/project")
        );
        assert_eq!(
            effective.provenance.user_workspace.kind,
            SessionConfigSourceKind::LegacyMigration
        );
        assert_eq!(effective.system_prompt_sha256.len(), 64);

        let channel_workspace = crate::config::WorkspaceBindingConfig {
            execution_target: "local".into(),
            path: "/channel/project".into(),
            read_only: true,
        };
        let channel_effective = resolve_session_config(
            &agents[0],
            &SessionConfigOverrides::default(),
            Some(("terminal", &channel_workspace)),
            Some(std::path::Path::new("/legacy/project")),
        )
        .unwrap();
        assert_eq!(
            channel_effective.user_workspace.unwrap().path,
            std::path::PathBuf::from("/channel/project")
        );
        assert_eq!(
            channel_effective.provenance.user_workspace.kind,
            SessionConfigSourceKind::ChannelDefault
        );

        let error = resolve_session_config(
            &agents[0],
            &SessionConfigOverrides {
                system_prompt: Some("session prompt".into()),
                ..SessionConfigOverrides::default()
            },
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(error, CompositionError::SessionPromptDisabled));
    }
}
