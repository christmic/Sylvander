//! Production composition of configured Agent runs.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use sylvander_agent::bus::MessageBus;
use sylvander_agent::prompt::{PromptProfile, PromptResolveError, PromptResolver};
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
use sylvander_llm_core::{
    ModelCapabilities as ProviderModelCapabilities, ModelInfo as ProviderModelInfo, ModelProvider,
    ModelRef,
};
use sylvander_protocol::{
    ApprovalPolicy, FileAccess, ModelSelection, ModelSelectionResolutionError, NetworkAccess,
    PermissionProfile, ReasoningEffort, SessionConfigOverrides, SessionConfigProvenance,
    SessionConfigSource, SessionConfigSourceKind, SessionEffectiveConfig, SessionWorkspaceBinding,
};

#[cfg(test)]
use crate::config::SystemSecretResolver;
use crate::config::{
    AgentDefinitionConfig, ModelDefinitionConfig, ModelProviderConfig, SecretResolver, ServerConfig,
};
use crate::credential_registry::CredentialSecretResolver;
#[cfg(test)]
use crate::registry_composition::RegistryCompositionSnapshot;
use crate::registry_composition_v3::VersionedRegistryCompositionSnapshot;
#[doc(hidden)]
pub use crate::registry_domain::ModelCapabilityIssue;
use crate::registry_domain::{
    CanonicalModelCapability, ModelDefinition, ProviderDefinition, parse_model_capabilities,
};
use crate::request_scoped_provider::{
    AnthropicProviderFactory, PinnedProviderRouter, ProviderAdapterFactory,
    RegistryCredentialSource,
};

/// A configured run plus the metadata needed by protocol adapters.
#[derive(Clone)]
struct RegistryRevisionBindings {
    provider_revisions: HashMap<String, u64>,
    model_revisions: HashMap<ModelSelection, u64>,
}

#[derive(Clone)]
pub struct ConfiguredAgent {
    pub spec: AgentSpec,
    pub run: AgentRun,
    pub models: BTreeMap<ModelSelection, ModelInfo>,
    pub approval_enabled: bool,
    pub definition: AgentDefinitionConfig,
    pub execution_targets: HashSet<String>,
    prompt_resolver: Arc<PromptResolver>,
    revision_bindings: Option<RegistryRevisionBindings>,
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

pub(crate) fn build_agent(
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

    let model_list = model_catalog(provider)?;
    let models = model_list
        .iter()
        .cloned()
        .map(|model| {
            (
                ModelSelection {
                    provider_id: provider.id.clone(),
                    model_id: model.id.clone(),
                },
                model,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let default_selection = ModelSelection {
        provider_id: provider.id.clone(),
        model_id: definition.spec.model.model_name.clone(),
    };
    let primary = models
        .get(&default_selection)
        .ok_or_else(|| CompositionError::MissingModel {
            provider: provider.id.clone(),
            model: definition.spec.model.model_name.clone(),
        })?;
    let prompt_resolver = configured_prompt_resolver(definition)?;
    let mut spec = definition.spec.clone();
    apply_default_prompt(&prompt_resolver, definition, &default_selection, &mut spec)?;

    let memory = Arc::new(InMemoryMemoryStore::new());
    let tools = default_tools(memory.clone());

    let builder = AgentRun::builder(spec.clone(), client)
        .bus(bus)
        .session_store(sessions)
        .memory(memory)
        .override_tools(tools)
        .available_models(model_list)
        .prompt_resolver(prompt_resolver.clone())
        .model_capabilities(primary.capabilities);
    let run = apply_server_run_settings(config, builder)
        .build()
        .map_err(|error| CompositionError::Agent(spec.id.to_string(), error.to_string()))?;

    Ok(ConfiguredAgent {
        spec,
        run,
        models,
        approval_enabled: config.server.approval.enabled,
        definition: definition.clone(),
        execution_targets: execution_targets(config),
        prompt_resolver,
        revision_bindings: None,
    })
}

/// Build one immutable registry revision while keeping credentials live.
#[cfg(test)]
pub(crate) fn build_registry_agent(
    config: &ServerConfig,
    snapshot: RegistryCompositionSnapshot,
    registry: crate::agent_registry::AgentRegistry,
    bus: Arc<dyn MessageBus>,
    sessions: Arc<dyn SessionStore>,
) -> Result<ConfiguredAgent, CompositionError> {
    build_registry_agent_with_resolver(
        config,
        snapshot,
        registry,
        bus,
        sessions,
        Arc::new(SystemSecretResolver),
    )
}

#[cfg(test)]
pub(crate) fn build_registry_agent_with_resolver(
    config: &ServerConfig,
    snapshot: RegistryCompositionSnapshot,
    registry: crate::agent_registry::AgentRegistry,
    bus: Arc<dyn MessageBus>,
    sessions: Arc<dyn SessionStore>,
    resolver: Arc<dyn CredentialSecretResolver>,
) -> Result<ConfiguredAgent, CompositionError> {
    let RegistryCompositionSnapshot {
        agent: definition,
        provider,
        models: definitions,
        default_model_id,
        credential_binding_id,
    } = snapshot;
    let revision_bindings = registry_revision_bindings(&provider, &definitions)?;
    if credential_binding_id != provider.credential_binding_id {
        return Err(CompositionError::RegistryBindingMismatch);
    }
    for model in &definitions {
        AnthropicProviderFactory
            .preflight(&provider, model)
            .map_err(|error| CompositionError::ProviderFactory(error.to_string()))?;
    }
    let credentials = Arc::new(RegistryCredentialSource::new(registry, resolver));
    let provider_adapter = AnthropicProviderFactory
        .create(provider.clone(), credentials)
        .map_err(|error| CompositionError::ProviderFactory(error.to_string()))?;
    let (models, provider_models) = registry_model_catalog(&definitions)?;
    let primary = provider_models
        .iter()
        .find(|model| model.reference.model == default_model_id)
        .cloned()
        .ok_or_else(|| CompositionError::MissingModel {
            provider: provider.id.clone(),
            model: default_model_id.clone(),
        })?;
    let default_selection = ModelSelection {
        provider_id: provider.id.clone(),
        model_id: default_model_id.clone(),
    };
    let prompt_resolver = configured_prompt_resolver(&definition)?;
    let mut spec = definition.spec.clone();
    apply_default_prompt(&prompt_resolver, &definition, &default_selection, &mut spec)?;

    let memory = Arc::new(InMemoryMemoryStore::new());
    let tools = default_tools(memory.clone());
    let lifecycles = definitions
        .iter()
        .map(|model| (model.model_id.clone(), model.lifecycle.clone()))
        .collect::<HashMap<_, _>>();
    let pricing = definitions
        .iter()
        .filter_map(|model| model.pricing.map(|value| (model.model_id.clone(), value)))
        .collect::<HashMap<_, _>>();
    let mut builder = AgentRun::provider_builder(spec.clone(), provider_adapter, primary)
        .bus(bus)
        .session_store(sessions)
        .memory(memory)
        .override_tools(tools)
        .available_provider_models(provider_models)
        .model_lifecycles(lifecycles)
        .model_pricing(pricing)
        .prompt_resolver(prompt_resolver.clone());
    builder = apply_server_run_settings(config, builder);
    let run = builder
        .build()
        .map_err(|error| CompositionError::Agent(spec.id.to_string(), error.to_string()))?;

    Ok(ConfiguredAgent {
        spec,
        run,
        models,
        approval_enabled: config.server.approval.enabled,
        definition,
        execution_targets: execution_targets(config),
        prompt_resolver,
        revision_bindings: Some(revision_bindings),
    })
}

/// Build one complete versioned registry closure around an immutable router.
#[allow(dead_code)] // wired into revision composition after the staged router batches
pub(crate) fn build_registry_agent_versioned_with_resolver(
    config: &ServerConfig,
    snapshot: VersionedRegistryCompositionSnapshot,
    registry: crate::agent_registry::AgentRegistry,
    bus: Arc<dyn MessageBus>,
    sessions: Arc<dyn SessionStore>,
    resolver: Arc<dyn CredentialSecretResolver>,
) -> Result<ConfiguredAgent, CompositionError> {
    let VersionedRegistryCompositionSnapshot {
        agent: definition,
        providers,
        models: model_definitions,
        default_model,
    } = snapshot;
    let revision_bindings = versioned_registry_revision_bindings(&providers, &model_definitions)?;
    for (selection, model) in &model_definitions {
        let provider = providers
            .get(&selection.provider_id)
            .ok_or_else(|| CompositionError::MissingProvider(selection.provider_id.clone()))?;
        AnthropicProviderFactory
            .preflight(provider, model)
            .map_err(|error| CompositionError::ProviderFactory(error.to_string()))?;
    }
    let credentials = Arc::new(RegistryCredentialSource::new(registry, resolver));
    let mut adapters_by_provider =
        HashMap::<String, Arc<dyn ModelProvider>>::with_capacity(providers.len());
    for (provider_id, provider) in providers {
        if provider.id != provider_id {
            return Err(CompositionError::InvalidRegistryRevisionBinding);
        }
        let adapter = AnthropicProviderFactory
            .create(provider, credentials.clone())
            .map_err(|error| CompositionError::ProviderFactory(error.to_string()))?;
        adapters_by_provider.insert(provider_id, adapter);
    }

    let definitions = model_definitions.into_values().collect::<Vec<_>>();
    let (models, provider_models) = registry_model_catalog(&definitions)?;
    let primary = provider_models
        .iter()
        .find(|model| {
            model.reference.provider == default_model.provider_id
                && model.reference.model == default_model.model_id
        })
        .cloned()
        .ok_or_else(|| CompositionError::MissingModel {
            provider: default_model.provider_id.clone(),
            model: default_model.model_id.clone(),
        })?;
    let model_catalog = provider_models
        .iter()
        .map(|model| (model.reference.clone(), model.capabilities))
        .collect::<HashMap<_, _>>();
    let router = PinnedProviderRouter::new(adapters_by_provider, model_catalog)
        .map_err(|error| CompositionError::ProviderRouter(error.to_string()))?;

    let prompt_resolver = configured_prompt_resolver(&definition)?;
    let mut spec = definition.spec.clone();
    apply_default_prompt(&prompt_resolver, &definition, &default_model, &mut spec)?;
    let memory = Arc::new(InMemoryMemoryStore::new());
    let tools = default_tools(memory.clone());
    let lifecycles = definitions
        .iter()
        .map(|model| {
            (
                ModelSelection {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                },
                model.lifecycle.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    let pricing = definitions
        .iter()
        .filter_map(|model| {
            model.pricing.map(|value| {
                (
                    ModelSelection {
                        provider_id: model.provider_id.clone(),
                        model_id: model.model_id.clone(),
                    },
                    value,
                )
            })
        })
        .collect::<HashMap<_, _>>();
    let builder = AgentRun::qualified_router_builder(spec.clone(), Arc::new(router), primary)
        .bus(bus)
        .session_store(sessions)
        .memory(memory)
        .override_tools(tools)
        .available_provider_models(provider_models)
        .qualified_model_lifecycles(lifecycles)
        .qualified_model_pricing(pricing)
        .prompt_resolver(prompt_resolver.clone());
    let run = apply_server_run_settings(config, builder)
        .build()
        .map_err(|error| CompositionError::Agent(spec.id.to_string(), error.to_string()))?;

    Ok(ConfiguredAgent {
        spec,
        run,
        models,
        approval_enabled: config.server.approval.enabled,
        definition,
        execution_targets: execution_targets(config),
        prompt_resolver,
        revision_bindings: Some(revision_bindings),
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
    let catalog = agent.models.keys().cloned().collect::<Vec<_>>();
    let selection = overrides
        .resolve_model_selection(&catalog)
        .map_err(CompositionError::ModelSelection)?
        .unwrap_or_else(|| ModelSelection {
            provider_id: definition.spec.model.provider.clone(),
            model_id: definition.spec.model.model_name.clone(),
        });
    let model = agent
        .models
        .get(&selection)
        .ok_or_else(|| CompositionError::MissingModel {
            provider: selection.provider_id.clone(),
            model: selection.model_id.clone(),
        })?;
    let provider_id = selection.provider_id.clone();
    let model_id = selection.model_id.clone();
    let (provider_revision, model_revision) = match &agent.revision_bindings {
        None => (None, None),
        Some(bindings) => {
            let provider_revision = bindings
                .provider_revisions
                .get(&provider_id)
                .ok_or(CompositionError::RegistryProviderBindingMismatch)?;
            let model_revision = bindings.model_revisions.get(&selection).ok_or_else(|| {
                CompositionError::MissingRegistryModelBinding {
                    provider: provider_id.clone(),
                    model: model_id.clone(),
                }
            })?;
            (Some(*provider_revision), Some(*model_revision))
        }
    };
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

    let resolved_prompt = agent
        .prompt_resolver
        .resolve(
            &selection,
            overrides.prompt_profile.as_deref(),
            overrides.system_prompt.as_deref(),
        )
        .map_err(|error| map_prompt_error(error, definition, &selection, overrides))?;

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
        provider_revision,
        model_id,
        model_revision,
        reasoning_effort,
        permissions,
        prompt_profile: resolved_prompt.profile_id,
        system_prompt_sha256: resolved_prompt.system_prompt_sha256,
        prompt_manifest: Some(resolved_prompt.manifest),
        agent_workspace,
        user_workspace,
        execution_target,
        provenance: SessionConfigProvenance {
            model: choose(
                overrides.model.is_some() || overrides.model_id.is_some(),
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

fn default_tools(memory: Arc<InMemoryMemoryStore>) -> ToolRegistry {
    ToolRegistry::new()
        .register(ReadTool::new("/"))
        .register(WriteTool::new("/"))
        .register(EditTool::new("/"))
        .register(MemoryReadTool::new(memory))
        .register(AskUserTool::new())
        .register(PresentPlanTool::new())
        .register(UpdatePlanTool::new())
        .register(StartBackgroundTaskTool::new())
}

fn configured_prompt_resolver(
    definition: &AgentDefinitionConfig,
) -> Result<Arc<PromptResolver>, CompositionError> {
    PromptResolver::new(
        format!("agent:{}@{}", definition.spec.id, definition.revision),
        definition.spec.persona.system_prompt.clone(),
        definition
            .prompt_profiles
            .iter()
            .map(|profile| PromptProfile {
                id: profile.id.clone(),
                providers: profile.providers.clone(),
                models: profile.models.clone(),
                system_prompt: profile.system_prompt.clone(),
            })
            .collect(),
        definition.default_prompt_profile.clone(),
        definition.allow_session_prompt,
    )
    .map(Arc::new)
    .map_err(|_| CompositionError::InvalidPrompt)
}

fn execution_targets(config: &ServerConfig) -> HashSet<String> {
    config
        .execution_targets
        .iter()
        .map(|target| target.id.clone())
        .chain(std::iter::once("local".into()))
        .collect()
}

fn apply_server_run_settings(
    config: &ServerConfig,
    mut builder: sylvander_agent::run::AgentRunBuilder,
) -> sylvander_agent::run::AgentRunBuilder {
    if let Some(path) = &config.server.workspace_journal {
        builder = builder.workspace_journal(path);
    }
    if config.server.approval.enabled {
        builder = builder.enable_approval();
    }
    if let Some(path) = &config.server.approval.persistent_store {
        builder = builder.approval_store(path);
    }
    builder
}

#[cfg(test)]
fn registry_revision_bindings(
    provider: &ProviderDefinition,
    models: &[ModelDefinition],
) -> Result<RegistryRevisionBindings, CompositionError> {
    if provider.id.trim().is_empty() || provider.revision == 0 {
        return Err(CompositionError::InvalidRegistryRevisionBinding);
    }
    let mut model_revisions = HashMap::with_capacity(models.len());
    for model in models {
        if model.model_id.trim().is_empty() || model.revision == 0 {
            return Err(CompositionError::InvalidRegistryRevisionBinding);
        }
        if model.provider_id != provider.id {
            return Err(CompositionError::RegistryModelProviderMismatch {
                provider: provider.id.clone(),
                model: model.model_id.clone(),
                model_provider: model.provider_id.clone(),
            });
        }
        let selection = ModelSelection {
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
        };
        if model_revisions
            .insert(selection.clone(), model.revision)
            .is_some()
        {
            return Err(CompositionError::DuplicateRegistryModelBinding {
                provider: selection.provider_id,
                model: selection.model_id,
            });
        }
    }
    Ok(RegistryRevisionBindings {
        provider_revisions: HashMap::from([(provider.id.clone(), provider.revision)]),
        model_revisions,
    })
}

fn versioned_registry_revision_bindings(
    providers: &BTreeMap<String, ProviderDefinition>,
    models: &BTreeMap<ModelSelection, ModelDefinition>,
) -> Result<RegistryRevisionBindings, CompositionError> {
    let mut provider_revisions = HashMap::with_capacity(providers.len());
    for (provider_id, provider) in providers {
        if provider_id.trim().is_empty()
            || provider.id != *provider_id
            || provider.revision == 0
            || provider_revisions
                .insert(provider_id.clone(), provider.revision)
                .is_some()
        {
            return Err(CompositionError::InvalidRegistryRevisionBinding);
        }
    }
    let mut model_revisions = HashMap::with_capacity(models.len());
    for (selection, model) in models {
        if selection.provider_id != model.provider_id
            || selection.model_id != model.model_id
            || model.revision == 0
            || !provider_revisions.contains_key(&selection.provider_id)
            || model_revisions
                .insert(selection.clone(), model.revision)
                .is_some()
        {
            return Err(CompositionError::InvalidRegistryRevisionBinding);
        }
    }
    Ok(RegistryRevisionBindings {
        provider_revisions,
        model_revisions,
    })
}

fn registry_model_catalog(
    definitions: &[ModelDefinition],
) -> Result<(BTreeMap<ModelSelection, ModelInfo>, Vec<ProviderModelInfo>), CompositionError> {
    let mut shadows = BTreeMap::new();
    let mut exact = Vec::with_capacity(definitions.len());
    for model in definitions {
        let (shadow_capabilities, provider_capabilities) = registry_model_capabilities(model)?;
        let shadow = ModelInfo::builder()
            .id(&model.model_id)
            .context_window(model.context_window)
            .max_output_tokens(model.max_output_tokens)
            .capabilities(shadow_capabilities)
            .build()
            .ok_or_else(|| CompositionError::InvalidModel(model.model_id.clone()))?;
        let selection = ModelSelection {
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
        };
        if shadows.insert(selection.clone(), shadow).is_some() {
            return Err(CompositionError::DuplicateRegistryModelBinding {
                provider: selection.provider_id,
                model: selection.model_id,
            });
        }
        exact.push(ProviderModelInfo {
            reference: ModelRef::new(&model.provider_id, &model.model_id),
            context_window: model.context_window,
            max_output_tokens: model.max_output_tokens,
            capabilities: provider_capabilities,
        });
    }
    Ok((shadows, exact))
}

fn registry_model_capabilities(
    model: &ModelDefinition,
) -> Result<(ModelCapabilities, ProviderModelCapabilities), CompositionError> {
    let capabilities = parse_model_capabilities(&model.capabilities).map_err(|error| {
        CompositionError::InvalidModelCapability {
            model: model.model_id.clone(),
            issue: error.issue(),
        }
    })?;
    Ok(canonical_model_capability_bits(capabilities))
}

fn canonical_model_capability_bits(
    capabilities: impl IntoIterator<Item = CanonicalModelCapability>,
) -> (ModelCapabilities, ProviderModelCapabilities) {
    let mut shadow = ModelCapabilities::empty();
    let mut exact = ProviderModelCapabilities::empty();
    for capability in capabilities {
        let (shadow_capability, exact_capability) = match capability {
            CanonicalModelCapability::ExtendedThinking => (
                ModelCapabilities::EXTENDED_THINKING,
                ProviderModelCapabilities::REASONING,
            ),
            CanonicalModelCapability::PromptCaching => (
                ModelCapabilities::PROMPT_CACHING,
                ProviderModelCapabilities::PROMPT_CACHING,
            ),
            CanonicalModelCapability::StructuredOutput => (
                ModelCapabilities::STRUCTURED_OUTPUT,
                ProviderModelCapabilities::STRUCTURED_OUTPUT,
            ),
            CanonicalModelCapability::ToolUse => (
                ModelCapabilities::TOOL_USE,
                ProviderModelCapabilities::TOOL_USE,
            ),
            CanonicalModelCapability::Vision => {
                (ModelCapabilities::VISION, ProviderModelCapabilities::VISION)
            }
            CanonicalModelCapability::DocumentInput => (
                ModelCapabilities::DOCUMENT_INPUT,
                ProviderModelCapabilities::DOCUMENT_INPUT,
            ),
        };
        shadow |= shadow_capability;
        exact = exact | exact_capability;
    }
    (shadow, exact)
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
    resolver: &PromptResolver,
    definition: &AgentDefinitionConfig,
    selection: &ModelSelection,
    spec: &mut AgentSpec,
) -> Result<(), CompositionError> {
    let composed = resolver.resolve(selection, None, None).map_err(|error| {
        map_prompt_error(
            error,
            definition,
            selection,
            &SessionConfigOverrides::default(),
        )
    })?;
    spec.persona.system_prompt = composed.system_prompt;
    Ok(())
}

fn map_prompt_error(
    error: PromptResolveError,
    definition: &AgentDefinitionConfig,
    selection: &ModelSelection,
    overrides: &SessionConfigOverrides,
) -> CompositionError {
    match error {
        PromptResolveError::Invalid => CompositionError::InvalidPrompt,
        PromptResolveError::MissingProfile => CompositionError::MissingPromptProfile {
            agent: definition.spec.id.to_string(),
            profile: overrides
                .prompt_profile
                .clone()
                .or_else(|| definition.default_prompt_profile.clone())
                .unwrap_or_else(|| "unknown".into()),
        },
        PromptResolveError::IncompatibleProfile => CompositionError::IncompatiblePromptProfile {
            profile: overrides
                .prompt_profile
                .clone()
                .or_else(|| definition.default_prompt_profile.clone())
                .unwrap_or_else(|| "unknown".into()),
            provider: selection.provider_id.clone(),
            model: selection.model_id.clone(),
        },
        PromptResolveError::SessionPromptDisabled => CompositionError::SessionPromptDisabled,
    }
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
    let capabilities = parse_model_capabilities(&model.capabilities).map_err(|error| {
        CompositionError::InvalidModelCapability {
            model: model.id.clone(),
            issue: error.issue(),
        }
    })?;
    Ok(canonical_model_capability_bits(capabilities).0)
}

#[derive(Debug, thiserror::Error)]
pub enum CompositionError {
    #[error("model provider `{0}` is unavailable")]
    MissingProvider(String),
    #[error("model `{model}` is unavailable from provider `{provider}`")]
    MissingModel { provider: String, model: String },
    #[error(transparent)]
    ModelSelection(#[from] ModelSelectionResolutionError),
    #[error("model `{0}` does not support reasoning")]
    UnsupportedReasoning(String),
    #[error("execution target `{0}` is unavailable")]
    MissingExecutionTarget(String),
    #[error("approval policy `ask` requires approvals to be enabled")]
    ApprovalDisabled,
    #[error("session system prompt overrides are disabled")]
    SessionPromptDisabled,
    #[error("prompt configuration is invalid")]
    InvalidPrompt,
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
    #[error("registry credential binding does not match the pinned Provider")]
    RegistryBindingMismatch,
    #[error("registry revision binding contains an empty identity or zero revision")]
    InvalidRegistryRevisionBinding,
    #[error("registry Provider binding does not match the selected Provider")]
    RegistryProviderBindingMismatch,
    #[error("model `{model}` belongs to Provider `{model_provider}`, not `{provider}`")]
    RegistryModelProviderMismatch {
        provider: String,
        model: String,
        model_provider: String,
    },
    #[error("registry Model binding `{provider}/{model}` is duplicated")]
    DuplicateRegistryModelBinding { provider: String, model: String },
    #[error("registry Model binding `{provider}/{model}` is missing")]
    MissingRegistryModelBinding { provider: String, model: String },
    #[error("failed to create pinned Provider: {0}")]
    ProviderFactory(String),
    #[error("failed to create pinned Provider router: {0}")]
    ProviderRouter(String),
    #[error("model `{0}` has invalid metadata")]
    InvalidModel(String),
    #[error("model `{model}` has invalid capability metadata: {issue}")]
    InvalidModelCapability {
        model: String,
        issue: ModelCapabilityIssue,
    },
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
    use sylvander_protocol::ModelSelection;

    #[test]
    fn capability_mapping_covers_the_canonical_vocabulary() {
        let model = ModelDefinition {
            provider_id: "provider".into(),
            model_id: "model".into(),
            revision: 1,
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities: [
                "extended_thinking",
                "prompt_caching",
                "structured_output",
                "tool_use",
                "vision",
                "document_input",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            lifecycle: sylvander_protocol::ModelLifecycle::Active,
            pricing: None,
        };

        let (shadow, exact) = registry_model_capabilities(&model).unwrap();

        assert_eq!(
            shadow,
            ModelCapabilities::EXTENDED_THINKING
                | ModelCapabilities::PROMPT_CACHING
                | ModelCapabilities::STRUCTURED_OUTPUT
                | ModelCapabilities::TOOL_USE
                | ModelCapabilities::VISION
                | ModelCapabilities::DOCUMENT_INPUT
        );
        assert_eq!(
            exact,
            ProviderModelCapabilities::REASONING
                | ProviderModelCapabilities::PROMPT_CACHING
                | ProviderModelCapabilities::STRUCTURED_OUTPUT
                | ProviderModelCapabilities::TOOL_USE
                | ProviderModelCapabilities::VISION
                | ProviderModelCapabilities::DOCUMENT_INPUT
        );
    }

    #[test]
    fn config_capability_mapping_uses_domain_aliases_and_fails_closed() {
        let mut model = ModelDefinitionConfig {
            id: "model".into(),
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities: vec!["reasoning".into()],
        };
        assert_eq!(
            model_capabilities(&model).unwrap(),
            ModelCapabilities::EXTENDED_THINKING
        );

        model.capabilities = vec!["telepathy".into()];
        assert!(matches!(
            model_capabilities(&model),
            Err(CompositionError::InvalidModelCapability {
                model,
                issue: ModelCapabilityIssue::Unknown
            }) if model == "model"
        ));

        let raw = "secret_future_capability";
        model.capabilities = vec![raw.into()];
        let error = model_capabilities(&model).unwrap_err();
        assert!(!error.to_string().contains(raw));
        assert!(!format!("{error:?}").contains(raw));
    }

    fn versioned_config() -> ServerConfig {
        ServerConfig::from_toml(
            r#"
schema_version = 1

[[model_providers]]
id = "alpha"
base_url = "https://alpha.invalid"
[model_providers.api_key]
source = "env"
name = "ALPHA_KEY"
[[model_providers.models]]
id = "shared"

[[model_providers]]
id = "beta"
base_url = "https://beta.invalid"
[model_providers.api_key]
source = "env"
name = "BETA_KEY"
[[model_providers.models]]
id = "shared"

[[agents]]
[agents.spec]
id = "assistant"
name = "Assistant"
[agents.spec.model]
provider = "alpha"
model_name = "shared"
"#,
        )
        .unwrap()
    }

    fn versioned_snapshot(config: &ServerConfig) -> VersionedRegistryCompositionSnapshot {
        let selection = |provider_id: &str| ModelSelection {
            provider_id: provider_id.into(),
            model_id: "shared".into(),
        };
        let model = |provider_id: &str, lifecycle| ModelDefinition {
            provider_id: provider_id.into(),
            model_id: "shared".into(),
            revision: if provider_id == "alpha" { 3 } else { 5 },
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities: ["tool_use".into()].into(),
            lifecycle,
            pricing: None,
        };
        VersionedRegistryCompositionSnapshot {
            agent: config.agents[0].clone(),
            providers: BTreeMap::from([
                (
                    "alpha".into(),
                    ProviderDefinition {
                        id: "alpha".into(),
                        revision: 2,
                        kind: "anthropic_compatible".into(),
                        base_url: "https://alpha.invalid".into(),
                        credential_binding_id: "alpha-key".into(),
                    },
                ),
                (
                    "beta".into(),
                    ProviderDefinition {
                        id: "beta".into(),
                        revision: 4,
                        kind: "anthropic_compatible".into(),
                        base_url: "https://beta.invalid".into(),
                        credential_binding_id: "beta-key".into(),
                    },
                ),
            ]),
            models: BTreeMap::from([
                (
                    selection("alpha"),
                    model("alpha", sylvander_protocol::ModelLifecycle::Active),
                ),
                (
                    selection("beta"),
                    model(
                        "beta",
                        sylvander_protocol::ModelLifecycle::Deprecated { replacement: None },
                    ),
                ),
            ]),
            default_model: selection("alpha"),
        }
    }

    #[tokio::test]
    async fn versioned_builder_preserves_the_full_qualified_catalog() {
        let config = versioned_config();
        let directory = tempfile::tempdir().unwrap();
        let registry =
            crate::agent_registry::AgentRegistry::open(directory.path().join("registry.db"))
                .await
                .unwrap();
        let bus: Arc<dyn MessageBus> = Arc::new(InProcessMessageBus::new());
        let sessions: Arc<dyn SessionStore> =
            Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());

        let configured = build_registry_agent_versioned_with_resolver(
            &config,
            versioned_snapshot(&config),
            registry,
            bus,
            sessions,
            Arc::new(crate::config::SystemSecretResolver),
        )
        .unwrap();
        let info = configured.run.runtime_model_info().await;

        assert_eq!(
            info.models
                .iter()
                .map(|model| (model.provider.as_str(), model.id.as_str()))
                .collect::<Vec<_>>(),
            vec![("alpha", "shared"), ("beta", "shared")]
        );
        assert!(matches!(
            info.models[1].lifecycle,
            sylvander_protocol::ModelLifecycle::Deprecated { .. }
        ));
        configured
            .run
            .select_qualified_model(
                ModelSelection {
                    provider_id: "beta".into(),
                    model_id: "shared".into(),
                },
                ReasoningEffort::Off,
            )
            .await
            .unwrap();

        let beta = resolve_session_config(
            &configured,
            &SessionConfigOverrides {
                model: Some(ModelSelection {
                    provider_id: "beta".into(),
                    model_id: "shared".into(),
                }),
                ..SessionConfigOverrides::default()
            },
            None,
            None,
        )
        .unwrap();
        assert_eq!(beta.provider_id, "beta");
        assert_eq!(beta.model_id, "shared");
        assert_eq!(beta.provider_revision, Some(4));
        assert_eq!(beta.model_revision, Some(5));

        assert!(matches!(
            resolve_session_config(
                &configured,
                &SessionConfigOverrides {
                    model_id: Some("shared".into()),
                    ..SessionConfigOverrides::default()
                },
                None,
                None,
            ),
            Err(CompositionError::ModelSelection(
                ModelSelectionResolutionError::LegacyAmbiguous { model_id, provider_ids }
            )) if model_id == "shared" && provider_ids == vec!["alpha", "beta"]
        ));
    }

    #[tokio::test]
    async fn versioned_builder_preflights_every_model_before_router_construction() {
        let config = versioned_config();
        let mut snapshot = versioned_snapshot(&config);
        snapshot
            .models
            .get_mut(&ModelSelection {
                provider_id: "beta".into(),
                model_id: "shared".into(),
            })
            .unwrap()
            .capabilities = ["future_secret_capability".into()].into();
        let directory = tempfile::tempdir().unwrap();
        let registry =
            crate::agent_registry::AgentRegistry::open(directory.path().join("registry.db"))
                .await
                .unwrap();
        let bus: Arc<dyn MessageBus> = Arc::new(InProcessMessageBus::new());
        let sessions: Arc<dyn SessionStore> =
            Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());

        let result = build_registry_agent_versioned_with_resolver(
            &config,
            snapshot,
            registry,
            bus,
            sessions,
            Arc::new(crate::config::SystemSecretResolver),
        );
        let Err(error) = result else {
            panic!("unsupported model capability must fail before router construction");
        };

        assert!(matches!(
            error,
            CompositionError::ProviderFactory(message)
                if message == "model capability is unsupported by provider adapter"
        ));
    }

    #[test]
    fn versioned_bindings_reject_a_partial_provider_closure() {
        let config = versioned_config();
        let mut snapshot = versioned_snapshot(&config);
        snapshot.providers.remove("beta");

        assert!(matches!(
            versioned_registry_revision_bindings(&snapshot.providers, &snapshot.models),
            Err(CompositionError::InvalidRegistryRevisionBinding)
        ));
    }

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

        let mut agents =
            build_agents(&config, bus, sessions, &crate::config::SystemSecretResolver).unwrap();

        assert_eq!(agents.len(), 1);
        assert_eq!(
            agents[0].spec.persona.system_prompt,
            format!(
                "{}\n\nOptimized system prompt",
                sylvander_agent::prompt::SHARED_SAFETY_PROMPT
            )
        );
        assert!(
            agents[0]
                .models
                .values()
                .next()
                .unwrap()
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
        assert_eq!(effective.provider_revision, None);
        assert_eq!(effective.model_revision, None);
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
        assert!(effective.prompt_manifest.is_some());

        let qualified = resolve_session_config(
            &agents[0],
            &SessionConfigOverrides {
                model: Some(ModelSelection {
                    provider_id: "primary".into(),
                    model_id: "model-a".into(),
                }),
                ..SessionConfigOverrides::default()
            },
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            qualified.provenance.model.kind,
            SessionConfigSourceKind::SessionOverride
        );
        let legacy_model = resolve_session_config(
            &agents[0],
            &SessionConfigOverrides {
                model_id: Some("model-a".into()),
                ..SessionConfigOverrides::default()
            },
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            legacy_model.provenance.model.kind,
            SessionConfigSourceKind::SessionOverride
        );

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

        agents[0].definition.allow_session_prompt = true;
        for invalid in [
            String::new(),
            "x".repeat(sylvander_agent::prompt::MAX_SESSION_PROMPT_BYTES + 1),
            "private\0prompt".into(),
        ] {
            let error = resolve_session_config(
                &agents[0],
                &SessionConfigOverrides {
                    system_prompt: Some(invalid.clone()),
                    ..SessionConfigOverrides::default()
                },
                None,
                None,
            )
            .unwrap_err();
            assert!(matches!(error, CompositionError::InvalidPrompt));
            if !invalid.is_empty() {
                assert!(!error.to_string().contains(&invalid));
            }
        }
    }
}

#[cfg(test)]
#[path = "registry_agent_composition_tests.rs"]
mod registry_agent_composition_tests;
