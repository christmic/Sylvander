//! Production composition of configured Agent runs.

use std::sync::Arc;

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
    })
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
    }
}
