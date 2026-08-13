//! Production composition of configured Agent runs.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::agent_definition::{AgentSpec, McpServerConfig, McpStreamableHttpConfig, ToolRef};
use crate::mcp::stdio::McpResultArtifactSink;
use crate::mcp::{SessionMcpBinding, SessionMcpRuntimeService};
use crate::observability::RuntimeObservability;
use crate::prompt_contract::{agent_model_selection, public_prompt_manifest};
use sylvander_agent::memory::curated::MemoryCandidateSink;
use sylvander_agent::memory::store::MemoryStore;
use sylvander_agent::prompt::{PromptProfile, PromptResolveError, PromptResolver};
use sylvander_agent::tool::ToolRegistry;
use sylvander_agent::tool::invocation::{RegistryBoundToolGateway, ToolInvocationGateway};
use sylvander_agent::tools::{
    AskUserTool, CommandTool, EditTool, GitTool, ListTool, MemoryReadTool, PresentPlanTool,
    ReadTool, SearchTool, StartBackgroundTaskTool, UpdatePlanTool, WriteTool,
};
use sylvander_agent::user_profile_provider::UserProfileProvider;
use sylvander_api::{
    ApprovalPolicy, FileAccess, ModelSelection, ModelSelectionResolutionError, NetworkAccess,
    PermissionProfile, ReasoningEffort, SessionConfigOverrides, SessionConfigProvenance,
    SessionConfigSource, SessionConfigSourceKind, SessionEffectiveConfig, SessionWorkspaceBinding,
    SessionWorkspaceMount, WorkspaceCapabilityPolicy, WorkspaceMountRole,
};
use sylvander_channel::MessageBus;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
#[cfg(test)]
use sylvander_llm_anthropic::{AnthropicProvider, api::client::AnthropicClient};
use sylvander_llm_core::{
    ModelCapabilities as ProviderModelCapabilities, ModelInfo as ProviderModelInfo, ModelProvider,
    ModelRef,
};

use crate::agent_run::{
    AgentRun, AgentRunError, AgentSessionIssuer, AuthenticatedSession,
    SessionInvocationGatewayFactory,
};
use crate::config::{AgentDefinitionConfig, ExecutionTransportConfig, ServerConfig};
#[cfg(test)]
use crate::config::{ModelDefinitionConfig, ModelProviderConfig, SecretResolver};
use crate::credential::audit::CredentialOperationAuditLedger;
use crate::credential::registry::CredentialSecretResolver;
use crate::execution::{
    ContainerExecutor, ContainerPersistentProcessEnvironment, ContainerResourcePolicy,
    ExecutionTargetRegistration, RuntimeExecutionService, SshExecutor,
};
use crate::guardian::runtime::WorkerToolGatewayFactory;
use crate::provider::request_scoped::{
    AnthropicProviderFactory, PinnedProviderRouter, ProviderAdapterFactory,
    RegistryCredentialSource, RenewableExternalSecretProvider,
};
use crate::registry::composition::VersionedRegistryCompositionSnapshot;
#[doc(hidden)]
pub use crate::registry::domain::ModelCapabilityIssue;
use crate::registry::domain::{
    CanonicalModelCapability, ModelDefinition, ProviderDefinition, parse_model_capabilities,
};
use crate::storage::artifact::RuntimeArtifactService;
use crate::storage::session::SqliteSessionStore;

/// A configured run plus the metadata needed by protocol adapters.
#[derive(Clone)]
struct RegistryRevisionBindings {
    provider_revisions: HashMap<String, u64>,
    model_revisions: HashMap<ModelSelection, u64>,
}

#[derive(Clone)]
pub struct ConfiguredAgent {
    pub spec: AgentSpec,
    pub(crate) run: AgentRun,
    session_issuer: AgentSessionIssuer,
    mcp_sessions: SessionMcpRuntimeService,
    tool_gateway_factory: Option<WorkerToolGatewayFactory>,
    pub models: BTreeMap<ModelSelection, ModelInfo>,
    pub approval_enabled: bool,
    pub definition: AgentDefinitionConfig,
    pub execution_targets: HashMap<String, ExecutionTransportConfig>,
    #[cfg(test)]
    memory_store: Arc<dyn MemoryStore>,
    prompt_resolver: Arc<PromptResolver>,
    revision_bindings: RegistryRevisionBindings,
}

/// Redacted, read-only metadata exposed to transport composition.
///
/// This deliberately contains no `AgentRun`, session issuer, prompt source,
/// credential binding, or mutable runtime control.
#[derive(Clone)]
pub struct ConfiguredAgentDescriptor {
    pub id: sylvander_api::AgentId,
    pub default_model: ModelSelection,
    pub models: BTreeMap<ModelSelection, ModelInfo>,
    pub approval_enabled: bool,
    pub platform: sylvander_api::PlatformSnapshot,
    platform_provider: AgentRun,
}

impl ConfiguredAgentDescriptor {
    #[must_use]
    pub fn platform_snapshot(&self) -> sylvander_api::PlatformSnapshot {
        self.platform_provider.platform_snapshot()
    }
}

impl ConfiguredAgent {
    pub(crate) fn descriptor(&self) -> ConfiguredAgentDescriptor {
        ConfiguredAgentDescriptor {
            id: self.spec.id.clone(),
            default_model: ModelSelection {
                provider_id: self.spec.model.provider.clone(),
                model_id: self.spec.model.model_name.clone(),
            },
            models: self.models.clone(),
            approval_enabled: self.approval_enabled,
            platform: self.run.platform_snapshot(),
            platform_provider: self.run.clone(),
        }
    }
}

/// Build every configured Agent without starting background tasks.
#[cfg(test)]
pub(crate) fn build_agents(
    config: &ServerConfig,
    bus: Arc<dyn MessageBus>,
    sessions: Arc<SqliteSessionStore>,
    memory: Arc<dyn MemoryStore>,
    user_profiles: Option<Arc<dyn UserProfileProvider>>,
    secrets: &dyn SecretResolver,
) -> Result<Vec<ConfiguredAgent>, CompositionError> {
    config
        .agents
        .iter()
        .map(|agent| {
            build_agent(
                config,
                agent,
                bus.clone(),
                sessions.clone(),
                memory.clone(),
                user_profiles.clone(),
                secrets,
            )
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn build_agent(
    config: &ServerConfig,
    definition: &AgentDefinitionConfig,
    bus: Arc<dyn MessageBus>,
    sessions: Arc<SqliteSessionStore>,
    memory: Arc<dyn MemoryStore>,
    user_profiles: Option<Arc<dyn UserProfileProvider>>,
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
    let provider_models = exact_model_catalog(provider)?;
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
    let revision_bindings = RegistryRevisionBindings {
        provider_revisions: HashMap::from([(provider.id.clone(), 1)]),
        model_revisions: models
            .keys()
            .cloned()
            .map(|selection| (selection, 1))
            .collect(),
    };
    let default_selection = ModelSelection {
        provider_id: provider.id.clone(),
        model_id: definition.spec.model.model_name.clone(),
    };
    let prompt_resolver = configured_prompt_resolver(definition)?;
    let mut spec = definition.spec.clone();
    apply_default_prompt(&prompt_resolver, definition, &default_selection, &mut spec)?;

    let tools = default_tools(memory.clone());

    let primary_exact = provider_models
        .iter()
        .find(|model| model.reference.model == default_selection.model_id)
        .cloned()
        .ok_or_else(|| CompositionError::MissingModel {
            provider: provider.id.clone(),
            model: definition.spec.model.model_name.clone(),
        })?;
    let mut builder = AgentRun::qualified_router_builder(
        spec.clone(),
        Arc::new(AnthropicProvider::new(&provider.id, client)),
        primary_exact,
    )
    .bus(bus)
    .session_store(sessions.clone())
    .workflow_store(sessions)
    .memory(memory.clone())
    .override_tools(tools)
    .available_provider_models(provider_models)
    .prompt_resolver(prompt_resolver.clone());
    if let Some(provider) = user_profiles {
        builder = builder.user_profile_provider(provider);
    }
    let execution_service = build_execution_service(config, |reference| {
        secrets.resolve(reference).map_err(|_| ())
    })?;
    builder = builder.execution_service(execution_service.clone());
    let (run, session_issuer) = apply_server_run_settings(config, builder)
        .build_with_session_issuer()
        .map_err(|error| CompositionError::Agent(spec.id.to_string(), error.to_string()))?;

    Ok(ConfiguredAgent {
        spec,
        run,
        session_issuer,
        mcp_sessions: SessionMcpRuntimeService::new(execution_service, None, None),
        tool_gateway_factory: None,
        models,
        approval_enabled: config.server.approval.enabled,
        definition: definition.clone(),
        execution_targets: execution_targets(config),
        #[cfg(test)]
        memory_store: memory,
        prompt_resolver,
        revision_bindings,
    })
}

/// Build one complete versioned registry closure around an immutable router.
#[allow(dead_code)] // wired into revision composition after the staged router batches
#[allow(clippy::too_many_arguments)] // explicit composition dependencies stay type-visible
pub(crate) async fn build_registry_agent_versioned_with_resolver(
    config: &ServerConfig,
    snapshot: VersionedRegistryCompositionSnapshot,
    registry: crate::registry::agent::AgentRegistry,
    bus: Arc<dyn MessageBus>,
    observability: RuntimeObservability,
    execution_service: RuntimeExecutionService,
    sessions: Arc<SqliteSessionStore>,
    memory: Arc<dyn MemoryStore>,
    user_profiles: Option<Arc<dyn UserProfileProvider>>,
    resolver: Arc<dyn CredentialSecretResolver>,
    external_secret_provider: Option<Arc<dyn RenewableExternalSecretProvider>>,
    credential_audit: Arc<CredentialOperationAuditLedger>,
    result_artifacts: Option<Arc<dyn McpResultArtifactSink>>,
    artifact_service: Option<RuntimeArtifactService>,
    tool_gateway_factory: Option<WorkerToolGatewayFactory>,
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
    let credentials = Arc::new(match external_secret_provider {
        Some(provider) => {
            RegistryCredentialSource::with_external_provider(registry, provider, credential_audit)
        }
        None => RegistryCredentialSource::new(registry, resolver.clone(), credential_audit),
    });
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
    let candidate_sink = tool_gateway_factory
        .as_ref()
        .map(WorkerToolGatewayFactory::candidate_sink);
    let curated_context = tool_gateway_factory
        .as_ref()
        .map(WorkerToolGatewayFactory::curated_context_provider);
    let tools = configured_tools(&spec, memory.clone(), candidate_sink);
    let invocation_gateway = match &tool_gateway_factory {
        Some(factory) => Some(
            factory
                .build(spec.id.clone(), tools.invocation_descriptors())
                .map_err(|_| CompositionError::CapabilityRouter)?,
        ),
        None => None,
    };
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
    let mut builder = AgentRun::qualified_router_builder(spec.clone(), Arc::new(router), primary)
        .bus(bus)
        .observability(observability)
        .execution_service(execution_service.clone())
        .session_store(sessions.clone())
        .workflow_store(sessions)
        .memory(memory.clone())
        .override_tools(tools)
        .available_provider_models(provider_models)
        .qualified_model_lifecycles(lifecycles)
        .qualified_model_pricing(pricing)
        .prompt_resolver(prompt_resolver.clone());
    if let Some(service) = artifact_service {
        builder = builder.artifact_service(service);
    }
    if let Some(provider) = user_profiles {
        builder = builder.user_profile_provider(provider);
    }
    if let Some(provider) = curated_context {
        builder = builder.curated_context_provider(provider);
    }
    if let Some(gateway) = invocation_gateway {
        builder = builder.invocation_gateway(gateway);
    }
    let (run, session_issuer) = apply_server_run_settings(config, builder)
        .build_with_session_issuer()
        .map_err(|error| CompositionError::Agent(spec.id.to_string(), error.to_string()))?;

    Ok(ConfiguredAgent {
        spec,
        run,
        session_issuer,
        mcp_sessions: SessionMcpRuntimeService::new(
            execution_service,
            Some(resolver),
            result_artifacts,
        ),
        tool_gateway_factory,
        models,
        approval_enabled: config.server.approval.enabled,
        definition,
        execution_targets: execution_targets(config),
        #[cfg(test)]
        memory_store: memory,
        prompt_resolver,
        revision_bindings,
    })
}

impl ConfiguredAgent {
    pub(crate) async fn attach_authenticated_session(
        &self,
        session_id: sylvander_api::SessionId,
        metadata: crate::session::SessionMetadata,
    ) -> Result<AuthenticatedSession, AgentRunError> {
        let binding = SessionMcpBinding {
            user_id: metadata.user_id.clone(),
            agent_id: self.spec.id.clone(),
            session_id: session_id.clone(),
            policy_revision: self.definition.revision,
        };
        let workspace_root = metadata.workspace.clone();
        let servers = self
            .definition
            .spec
            .tools
            .iter()
            .filter_map(|reference| match reference {
                ToolRef::McpServer(server) => Some(server.clone()),
                ToolRef::Builtin { .. } | ToolRef::McpStreamableHttp(_) => None,
            })
            .collect();
        let http_servers = self
            .definition
            .spec
            .tools
            .iter()
            .filter_map(|reference| match reference {
                ToolRef::McpStreamableHttp(server) => Some(server.clone()),
                ToolRef::Builtin { .. } | ToolRef::McpServer(_) => None,
            })
            .collect();
        let lease = self.session_issuer.issue(session_id.clone(), metadata)?;
        self.attach_session_lease(lease, binding, servers, http_servers, workspace_root)
            .await
    }

    pub(crate) async fn attach_agent_instance(
        &self,
        session_id: sylvander_api::SessionId,
        agent_instance_id: sylvander_api::AgentInstanceId,
        metadata: crate::session::SessionMetadata,
    ) -> Result<AuthenticatedSession, AgentRunError> {
        if let Some(context) = self
            .run
            .get_agent_session(&session_id, &agent_instance_id)
            .await
        {
            return Ok(self
                .run
                .authenticated_session_handle(context.session_id, context.agent_instance_id));
        }
        let binding = SessionMcpBinding {
            user_id: metadata.user_id.clone(),
            agent_id: self.spec.id.clone(),
            session_id: session_id.clone(),
            policy_revision: self.definition.revision,
        };
        let workspace_root = metadata.workspace.clone();
        let servers = self
            .definition
            .spec
            .tools
            .iter()
            .filter_map(|reference| match reference {
                ToolRef::McpServer(server) => Some(server.clone()),
                ToolRef::Builtin { .. } | ToolRef::McpStreamableHttp(_) => None,
            })
            .collect();
        let http_servers = self
            .definition
            .spec
            .tools
            .iter()
            .filter_map(|reference| match reference {
                ToolRef::McpStreamableHttp(server) => Some(server.clone()),
                ToolRef::Builtin { .. } | ToolRef::McpServer(_) => None,
            })
            .collect();
        let lease = self.session_issuer.issue_for_agent_instance(
            session_id,
            agent_instance_id,
            metadata,
        )?;
        self.attach_session_lease(lease, binding, servers, http_servers, workspace_root)
            .await
    }

    async fn attach_session_lease(
        &self,
        lease: crate::agent_run::AuthenticatedSessionLease,
        binding: SessionMcpBinding,
        servers: Vec<McpServerConfig>,
        http_servers: Vec<McpStreamableHttpConfig>,
        workspace_root: std::path::PathBuf,
    ) -> Result<AuthenticatedSession, AgentRunError> {
        let session_id = binding.session_id.clone();
        let session = self.run.attach_authenticated_session(lease).await?;
        if self.mcp_sessions.tool_registry(&session_id).is_none()
            && let Err(error) = self
                .mcp_sessions
                .attach(binding, servers, http_servers, workspace_root)
                .await
        {
            self.run.leave_session(&session_id).await;
            return Err(AgentRunError::Configuration(error.to_string()));
        }
        let extensions = self.mcp_sessions.tool_registry(&session_id).ok_or_else(|| {
            AgentRunError::Configuration("MCP Session catalog is unavailable".into())
        });
        let extensions = match extensions {
            Ok(extensions) => extensions,
            Err(error) => {
                self.mcp_sessions.detach(&session_id).await;
                self.run.leave_session(&session_id).await;
                return Err(error);
            }
        };
        let agent_id = self.spec.id.clone();
        let invocation_gateway_factory: SessionInvocationGatewayFactory =
            match &self.tool_gateway_factory {
                Some(factory) => {
                    let factory = factory.clone();
                    Arc::new(move |descriptors| {
                        factory
                            .build(agent_id.clone(), descriptors)
                            .map_err(|error| AgentRunError::Configuration(error.to_string()))
                    })
                }
                None => Arc::new(move |descriptors| {
                    let gateway: Arc<dyn ToolInvocationGateway> =
                        RegistryBoundToolGateway::new(descriptors);
                    Ok(gateway)
                }),
            };
        if let Err(error) = self
            .run
            .install_session_tool_extensions(
                session_id.clone(),
                extensions,
                invocation_gateway_factory,
            )
            .await
        {
            self.mcp_sessions.detach(&session_id).await;
            self.run.leave_session(&session_id).await;
            return Err(error);
        }
        Ok(session)
    }

    pub(crate) async fn detach_authenticated_session(&self, session_id: &sylvander_api::SessionId) {
        self.mcp_sessions.detach(session_id).await;
        self.run.leave_session(session_id).await;
    }

    #[cfg(test)]
    pub(crate) fn uses_memory_store(&self, store: &Arc<dyn MemoryStore>) -> bool {
        Arc::ptr_eq(&self.memory_store, store)
    }
}

/// Resolve sparse session overrides against one immutable Agent definition.
pub fn resolve_session_config(
    agent: &ConfiguredAgent,
    overrides: &SessionConfigOverrides,
    channel_workspace: Option<(&str, &crate::config::WorkspaceBindingConfig)>,
    request_workspace_path: Option<&std::path::Path>,
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
    let provider_revision = agent
        .revision_bindings
        .provider_revisions
        .get(&provider_id)
        .copied()
        .ok_or(CompositionError::RegistryProviderBindingMismatch)?;
    let model_revision = agent
        .revision_bindings
        .model_revisions
        .get(&selection)
        .copied()
        .ok_or_else(|| CompositionError::MissingRegistryModelBinding {
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

    let resolved_prompt = agent
        .prompt_resolver
        .resolve(
            &agent_model_selection(&selection),
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
            request_workspace_path.map(|path| SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: path.to_path_buf(),
                read_only: false,
                instruction_focus: None,
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
    if !agent.execution_targets.contains_key(&execution_target) {
        return Err(CompositionError::MissingExecutionTarget(execution_target));
    }
    if user_workspace
        .as_ref()
        .or(agent_workspace.as_ref())
        .is_some_and(|workspace| workspace.execution_target != execution_target)
    {
        return Err(CompositionError::WorkspaceExecutionTargetMismatch);
    }
    validate_local_workspace_root(&agent.execution_targets, agent_workspace.as_ref())?;
    validate_local_workspace_root(&agent.execution_targets, user_workspace.as_ref())?;
    let workspace_mounts = compose_workspace_mounts(
        definition,
        agent_workspace.as_ref(),
        user_workspace.as_ref(),
    )?;
    for mount in &workspace_mounts {
        validate_local_workspace_root(&agent.execution_targets, Some(&mount.binding))?;
    }
    let agent_default = source(SessionConfigSourceKind::AgentDefault, &definition.spec.id.0);
    let session_override = source(SessionConfigSourceKind::SessionOverride, "session");
    let request_workspace = source(
        SessionConfigSourceKind::RequestOverride,
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
        prompt_manifest: public_prompt_manifest(resolved_prompt.manifest),
        agent_workspace,
        user_workspace,
        workspace_mounts,
        execution_target,
        provenance: SessionConfigProvenance {
            model: choose(overrides.model.is_some(), &session_override, &agent_default),
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
            } else if request_workspace_path.is_some() {
                request_workspace.clone()
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
            } else if overrides.user_workspace.is_none() && request_workspace_path.is_some() {
                request_workspace
            } else {
                agent_default
            },
        },
    })
}

fn compose_workspace_mounts(
    definition: &AgentDefinitionConfig,
    agent_workspace: Option<&SessionWorkspaceBinding>,
    user_workspace: Option<&SessionWorkspaceBinding>,
) -> Result<Vec<SessionWorkspaceMount>, CompositionError> {
    let mut mounts = Vec::with_capacity(definition.workspace_mounts.len() + 2);
    if let Some(binding) = agent_workspace {
        mounts.push(SessionWorkspaceMount {
            reference: "agent".into(),
            role: WorkspaceMountRole::AgentHome,
            binding: binding.clone(),
            capabilities: WorkspaceCapabilityPolicy {
                read: true,
                write: !binding.read_only,
                command: false,
                git: false,
            },
        });
    }
    if let Some(binding) = user_workspace {
        mounts.push(SessionWorkspaceMount {
            reference: "task".into(),
            role: WorkspaceMountRole::Task,
            binding: binding.clone(),
            capabilities: WorkspaceCapabilityPolicy {
                read: true,
                write: !binding.read_only,
                command: !binding.read_only,
                git: true,
            },
        });
    }
    mounts.extend(
        definition
            .workspace_mounts
            .iter()
            .map(|mount| SessionWorkspaceMount {
                reference: mount.reference.clone(),
                role: mount.role,
                binding: workspace_binding(&mount.binding),
                capabilities: mount.capabilities,
            }),
    );

    validate_workspace_mounts(&mounts)?;
    Ok(mounts)
}

fn validate_workspace_mounts(mounts: &[SessionWorkspaceMount]) -> Result<(), CompositionError> {
    let mut references = std::collections::HashSet::new();
    let mut locations = std::collections::HashMap::new();
    for mount in mounts {
        let reference = mount.reference.trim();
        if reference.is_empty()
            || reference.len() > 48
            || !reference.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err(CompositionError::InvalidWorkspaceMountReference(
                mount.reference.clone(),
            ));
        }
        if !references.insert(reference.to_owned()) {
            return Err(CompositionError::DuplicateWorkspaceMountReference(
                reference.to_owned(),
            ));
        }
        let location = (
            mount.binding.execution_target.clone(),
            mount.binding.path.clone(),
        );
        if let Some(existing_role) = locations.insert(location, mount.role) {
            let agent_task_alias = matches!(
                (existing_role, mount.role),
                (WorkspaceMountRole::AgentHome, WorkspaceMountRole::Task)
                    | (WorkspaceMountRole::Task, WorkspaceMountRole::AgentHome)
            );
            if !agent_task_alias {
                return Err(CompositionError::DuplicateWorkspaceMountLocation(
                    reference.to_owned(),
                ));
            }
        }
        if mount.binding.read_only && (mount.capabilities.write || mount.capabilities.command) {
            return Err(CompositionError::WorkspaceMountCapabilityConflict(
                reference.to_owned(),
            ));
        }
        if !mount.capabilities.read
            && (mount.capabilities.write || mount.capabilities.command || mount.capabilities.git)
        {
            return Err(CompositionError::WorkspaceMountCapabilityConflict(
                reference.to_owned(),
            ));
        }
    }
    Ok(())
}

fn workspace_binding(workspace: &crate::config::WorkspaceBindingConfig) -> SessionWorkspaceBinding {
    SessionWorkspaceBinding {
        execution_target: workspace.execution_target.clone(),
        path: workspace.path.clone().into(),
        read_only: workspace.read_only,
        instruction_focus: workspace.instruction_focus.clone().map(Into::into),
    }
}

#[cfg(test)]
pub(crate) fn default_tools(memory: Arc<dyn MemoryStore>) -> ToolRegistry {
    default_tools_with_candidate(memory, None)
}

fn default_tools_with_candidate(
    memory: Arc<dyn MemoryStore>,
    candidate_sink: Option<Arc<dyn MemoryCandidateSink>>,
) -> ToolRegistry {
    let registry = ToolRegistry::new()
        .register(ReadTool::new())
        .register(ListTool::new())
        .register(SearchTool::new())
        .register(WriteTool::new())
        .register(EditTool::new())
        .register(CommandTool::new())
        .register(GitTool::new())
        .register(MemoryReadTool::new(memory))
        .register(AskUserTool::new())
        .register(PresentPlanTool::new())
        .register(UpdatePlanTool::new())
        .register(sylvander_agent::tools::InspectRuntimeTool::new())
        .register(sylvander_agent::tools::ManageWorkflowTool::new())
        .register(StartBackgroundTaskTool::new());
    match candidate_sink {
        Some(sink) => registry.register(sylvander_agent::tools::MemoryWriteTool::candidate(sink)),
        None => registry,
    }
}

fn configured_tools(
    spec: &AgentSpec,
    memory: Arc<dyn MemoryStore>,
    candidate_sink: Option<Arc<dyn MemoryCandidateSink>>,
) -> ToolRegistry {
    // MCP declarations attach to authenticated Sessions. Starting them here
    // would recreate the revision-wide process sharing this architecture is
    // explicitly removing.
    default_tools_with_candidate(memory, candidate_sink).with_hooks(spec.hooks.clone())
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
                qualified_models: profile
                    .qualified_models
                    .iter()
                    .map(agent_model_selection)
                    .collect(),
                system_prompt: profile.system_prompt.clone(),
            })
            .collect(),
        definition.default_prompt_profile.clone(),
        definition.allow_session_prompt,
    )
    .map(Arc::new)
    .map_err(|_| CompositionError::InvalidPrompt)
}

fn execution_targets(config: &ServerConfig) -> HashMap<String, ExecutionTransportConfig> {
    let mut targets = config
        .execution_targets
        .iter()
        .map(|target| (target.id.clone(), target.transport.clone()))
        .collect::<HashMap<_, _>>();
    targets
        .entry("local".into())
        .or_insert(ExecutionTransportConfig::Local { root: None });
    targets
}

fn validate_local_workspace_root(
    targets: &HashMap<String, ExecutionTransportConfig>,
    workspace: Option<&SessionWorkspaceBinding>,
) -> Result<(), CompositionError> {
    let Some(workspace) = workspace else {
        return Ok(());
    };
    let target = targets.get(&workspace.execution_target).ok_or_else(|| {
        CompositionError::MissingExecutionTarget(workspace.execution_target.clone())
    })?;
    let (ExecutionTransportConfig::Local { root: Some(root) }
    | ExecutionTransportConfig::MacosSeatbelt {
        root: Some(root), ..
    }) = target
    else {
        return Ok(());
    };
    if !workspace.path.is_absolute() || !workspace.path.starts_with(root) {
        return Err(CompositionError::WorkspaceOutsideExecutionRoot {
            workspace: workspace.path.display().to_string(),
            root: root.display().to_string(),
        });
    }
    if workspace
        .path
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(CompositionError::WorkspaceOutsideExecutionRoot {
            workspace: workspace.path.display().to_string(),
            root: root.display().to_string(),
        });
    }
    Ok(())
}

fn apply_server_run_settings(
    config: &ServerConfig,
    mut builder: crate::agent_run::AgentRunBuilder,
) -> crate::agent_run::AgentRunBuilder {
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

pub(crate) fn build_execution_service(
    config: &ServerConfig,
    resolve: impl Fn(&crate::config::SecretRef) -> Result<crate::config::SecretValue, ()>,
) -> Result<RuntimeExecutionService, CompositionError> {
    // `local` is Runtime's built-in host adapter and remains an exact target;
    // it is not a fallback for any other identifier.
    let mut executors = HashMap::from([(
        "local".to_owned(),
        ExecutionTargetRegistration::local("local"),
    )]);
    for target in &config.execution_targets {
        let registration = match &target.transport {
            ExecutionTransportConfig::Ssh {
                host,
                port,
                user,
                credential,
                known_hosts,
                control_path,
                worktree_root: _,
            } => {
                let identity = resolve(credential)
                    .map_err(|()| CompositionError::ExecutionTarget(target.id.clone()))?;
                let identity_path = identity
                    .as_str()
                    .map_err(|_| CompositionError::ExecutionTarget(target.id.clone()))?;
                let executor = Arc::new(
                    SshExecutor::new(host, *port, user, identity_path, known_hosts, control_path)
                        .map_err(|_| CompositionError::ExecutionTarget(target.id.clone()))?,
                );
                ExecutionTargetRegistration::ssh(target.id.clone(), executor)
            }
            ExecutionTransportConfig::Container {
                runtime,
                image,
                resources,
            } => {
                let executor = Arc::new(
                    ContainerExecutor::new(runtime, image)
                        .and_then(|executor| {
                            executor.with_resource_policy(ContainerResourcePolicy {
                                memory_mb: resources.memory_mb,
                                cpu_millis: resources.cpu_millis,
                                pids_limit: resources.pids_limit,
                            })
                        })
                        .map_err(|_| CompositionError::ExecutionTarget(target.id.clone()))?,
                );
                let persistent_processes = Arc::new(
                    ContainerPersistentProcessEnvironment::new(target.id.clone(), runtime, image)
                        .map_err(|_| CompositionError::ExecutionTarget(target.id.clone()))?,
                );
                ExecutionTargetRegistration::container(
                    target.id.clone(),
                    executor,
                    persistent_processes,
                )
            }
            ExecutionTransportConfig::Local { .. } => {
                ExecutionTargetRegistration::local(target.id.clone())
            }
            ExecutionTransportConfig::MacosSeatbelt {
                allow_local_fallback,
                ..
            } => select_macos_execution_target(
                &target.id,
                *allow_local_fallback,
                macos_seatbelt_available(),
            )?,
            ExecutionTransportConfig::ClientWorker {
                channel_instance_id,
            } => ExecutionTargetRegistration::client_worker(
                target.id.clone(),
                channel_instance_id,
                Arc::new(crate::execution::WorkspaceWorkerExecutor::new(
                    target.id.clone(),
                )),
            ),
        };
        executors.insert(target.id.clone(), registration);
    }
    RuntimeExecutionService::new(executors.into_values())
        .map_err(|_| CompositionError::ExecutionTarget("registry".into()))
}

#[cfg(target_os = "macos")]
fn macos_seatbelt_available() -> bool {
    crate::execution::MacosSeatbeltExecutor::available()
}

#[cfg(not(target_os = "macos"))]
fn macos_seatbelt_available() -> bool {
    false
}

fn select_macos_execution_target(
    target_id: &str,
    allow_local_fallback: bool,
    seatbelt_available: bool,
) -> Result<ExecutionTargetRegistration, CompositionError> {
    if seatbelt_available {
        #[cfg(target_os = "macos")]
        return Ok(ExecutionTargetRegistration::macos_seatbelt(target_id));
    }
    if allow_local_fallback {
        return Ok(ExecutionTargetRegistration::trusted_local_fallback(
            target_id,
        ));
    }
    Err(CompositionError::ExecutionTarget(target_id.to_owned()))
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
            CanonicalModelCapability::AudioInput => (
                ModelCapabilities::AUDIO_INPUT,
                ProviderModelCapabilities::AUDIO_INPUT,
            ),
        };
        shadow |= shadow_capability;
        exact |= exact_capability;
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
    let composed = resolver
        .resolve(&agent_model_selection(selection), None, None)
        .map_err(|error| {
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

#[cfg(test)]
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

#[cfg(test)]
fn exact_model_catalog(
    provider: &ModelProviderConfig,
) -> Result<Vec<ProviderModelInfo>, CompositionError> {
    provider
        .models
        .iter()
        .map(|model| {
            let capabilities = parse_model_capabilities(&model.capabilities).map_err(|error| {
                CompositionError::InvalidModelCapability {
                    model: model.id.clone(),
                    issue: error.issue(),
                }
            })?;
            Ok(ProviderModelInfo {
                reference: ModelRef::new(&provider.id, &model.id),
                context_window: model.context_window,
                max_output_tokens: model.max_output_tokens,
                capabilities: canonical_model_capability_bits(capabilities).1,
            })
        })
        .collect()
}

#[cfg(test)]
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
    #[error("execution target `{0}` could not be initialized")]
    ExecutionTarget(String),
    #[error("workspace `{workspace}` is outside local execution root `{root}`")]
    WorkspaceOutsideExecutionRoot { workspace: String, root: String },
    #[error("workspace and session execution targets do not match")]
    WorkspaceExecutionTargetMismatch,
    #[error("workspace mount reference `{0}` is invalid")]
    InvalidWorkspaceMountReference(String),
    #[error("workspace mount reference `{0}` is duplicated")]
    DuplicateWorkspaceMountReference(String),
    #[error("workspace mount `{0}` duplicates another execution target and path")]
    DuplicateWorkspaceMountLocation(String),
    #[error("workspace mount `{0}` has capabilities that conflict with its binding")]
    WorkspaceMountCapabilityConflict(String),
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
    #[error("registry revision binding contains an empty identity or zero revision")]
    InvalidRegistryRevisionBinding,
    #[error("registry Provider binding does not match the selected Provider")]
    RegistryProviderBindingMismatch,
    #[error("registry Model binding `{provider}/{model}` is duplicated")]
    DuplicateRegistryModelBinding { provider: String, model: String },
    #[error("registry Model binding `{provider}/{model}` is missing")]
    MissingRegistryModelBinding { provider: String, model: String },
    #[error("failed to create pinned Provider: {0}")]
    ProviderFactory(String),
    #[error("failed to create pinned Provider router: {0}")]
    ProviderRouter(String),
    #[error("failed to start MCP server `{0}`: {1}")]
    Mcp(String, String),
    #[error("failed to build Agent capability router")]
    CapabilityRouter,
    #[error("failed to resolve MCP server `{0}` environment `{1}`")]
    McpSecret(String, String),
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
#[path = "../tests/unit/composition.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/unit/registry_agent_composition.rs"]
mod registry_agent_composition_tests;
