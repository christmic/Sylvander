//! Construction and dependency assembly for Runtime-owned Agent runs.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};

use tokio::sync::{Mutex, RwLock};

use sylvander_agent::compress::pipeline::CompressionPipeline;
use sylvander_agent::kernel::agent_loop::AgentLoop;
use sylvander_agent::memory::curated::CuratedContextProvider;
use sylvander_agent::memory::store::MemoryStore;
use sylvander_agent::prompt::PromptResolver;
use sylvander_agent::tool::ToolRegistry;
use sylvander_agent::tool::invocation::{RegistryBoundToolGateway, ToolInvocationGateway};
use sylvander_agent::turn_context::TurnContextBudgets;
use sylvander_agent::user_profile_provider::UserProfileProvider;
use sylvander_channel::MessageBus;
use sylvander_llm_core::{ModelInfo, ModelProvider};

use super::{
    AgentRun, AgentRunError, AgentRunInner, AgentSessionIssuer, MemorySource, RuntimeModel,
    RuntimeModels, SessionAuthorityMarker,
};
use crate::agent::approval::ApprovalMemory;
use crate::agent_definition::AgentSpec;
use crate::execution::RuntimeExecutionService;
use crate::observability::RuntimeObservability;
use crate::storage::artifact::RuntimeArtifactService;
use crate::storage::session::SessionStore;
use crate::storage::session::SqliteSessionStore;
use crate::storage::workspace_journal::WorkspaceJournal;

/// Builder for [`AgentRun`].
pub struct AgentRunBuilder {
    spec: AgentSpec,
    router: Arc<dyn ModelProvider>,
    model: ModelInfo,
    bus: Option<Arc<dyn MessageBus>>,
    observability: RuntimeObservability,
    tool_overrides: Option<ToolRegistry>,
    compression_overrides: Option<CompressionPipeline>,
    memory: Option<Arc<dyn MemoryStore>>,
    session_store: Option<Arc<dyn SessionStore>>,
    workflow_store: Option<Arc<SqliteSessionStore>>,
    available_provider_models: Vec<ModelInfo>,
    qualified_model_lifecycles:
        HashMap<sylvander_api::ModelSelection, sylvander_api::ModelLifecycle>,
    qualified_model_pricing: HashMap<sylvander_api::ModelSelection, sylvander_api::ModelPricing>,
    prompt_resolver: Option<Arc<PromptResolver>>,
    user_profile_provider: Option<Arc<dyn UserProfileProvider>>,
    curated_context_provider: Option<Arc<dyn CuratedContextProvider>>,
    turn_context_budgets: TurnContextBudgets,
    approval_enabled: bool,
    approval_rules: Vec<sylvander_agent::approval::ApprovalRule>,
    approval_store_path: Option<PathBuf>,
    workspace_journal_path: Option<PathBuf>,
    execution_service: Option<RuntimeExecutionService>,
    artifact_service: Option<RuntimeArtifactService>,
    invocation_gateway: Option<Arc<dyn ToolInvocationGateway>>,
}

impl AgentRunBuilder {
    pub(super) fn new_qualified_router(
        spec: AgentSpec,
        router: Arc<dyn ModelProvider>,
        model: ModelInfo,
    ) -> Self {
        Self {
            spec,
            router,
            model,
            bus: None,
            observability: RuntimeObservability::new(),
            tool_overrides: None,
            compression_overrides: None,
            memory: None,
            session_store: None,
            workflow_store: None,
            available_provider_models: Vec::new(),
            qualified_model_lifecycles: HashMap::new(),
            qualified_model_pricing: HashMap::new(),
            prompt_resolver: None,
            user_profile_provider: None,
            curated_context_provider: None,
            turn_context_budgets: TurnContextBudgets::default(),
            approval_enabled: false,
            approval_rules: Vec::new(),
            approval_store_path: None,
            workspace_journal_path: None,
            execution_service: Some(RuntimeExecutionService::standalone_local()),
            artifact_service: None,
            invocation_gateway: None,
        }
    }

    #[must_use]
    pub fn bus(mut self, bus: Arc<dyn MessageBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Inject the one Runtime-owned built-in lifecycle recorder.
    #[must_use]
    pub(crate) fn observability(mut self, observability: RuntimeObservability) -> Self {
        self.observability = observability;
        self
    }

    #[must_use]
    pub fn memory(mut self, store: Arc<dyn MemoryStore>) -> Self {
        self.memory = Some(store);
        self
    }

    #[must_use]
    pub fn session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Attach the authoritative coordination backend used by Agent workflow
    /// intents. Test-only Session stores may intentionally omit this port.
    #[must_use]
    pub(crate) fn workflow_store(mut self, store: Arc<SqliteSessionStore>) -> Self {
        self.workflow_store = Some(store);
        self
    }

    /// Attach Runtime's encrypted artifact service for every admitted turn.
    #[must_use]
    pub(crate) fn artifact_service(mut self, service: RuntimeArtifactService) -> Self {
        self.artifact_service = Some(service);
        self
    }

    #[must_use]
    pub fn override_tools(mut self, tools: ToolRegistry) -> Self {
        self.tool_overrides = Some(tools);
        self
    }

    /// Inject the Runtime-owned authorization and durable audit boundary used
    /// by every ordinary tool invocation.
    #[must_use]
    pub fn invocation_gateway(mut self, gateway: Arc<dyn ToolInvocationGateway>) -> Self {
        self.invocation_gateway = Some(gateway);
        self
    }

    /// Register exact provider-qualified models. The configured provider
    /// adapter can route only entries belonging to its own provider.
    #[must_use]
    pub fn available_provider_models(mut self, models: Vec<ModelInfo>) -> Self {
        self.available_provider_models = models;
        self
    }

    /// Attach lifecycle truth to exact provider-qualified models.
    #[must_use]
    pub fn qualified_model_lifecycles(
        mut self,
        lifecycles: HashMap<sylvander_api::ModelSelection, sylvander_api::ModelLifecycle>,
    ) -> Self {
        self.qualified_model_lifecycles = lifecycles;
        self
    }

    /// Attach pricing snapshots to exact provider-qualified models.
    #[must_use]
    pub fn qualified_model_pricing(
        mut self,
        pricing: HashMap<sylvander_api::ModelSelection, sylvander_api::ModelPricing>,
    ) -> Self {
        self.qualified_model_pricing = pricing;
        self
    }

    /// Attach the same immutable prompt resolver used by session composition.
    #[must_use]
    pub fn prompt_resolver(mut self, resolver: Arc<PromptResolver>) -> Self {
        self.prompt_resolver = Some(resolver);
        self
    }

    /// Inject Runtime-owned live profile lookup for each authenticated turn.
    #[must_use]
    pub fn user_profile_provider(mut self, provider: Arc<dyn UserProfileProvider>) -> Self {
        self.user_profile_provider = Some(provider);
        self
    }

    /// Inject committed Guardian output for bounded typed turn retrieval.
    #[must_use]
    pub fn curated_context_provider(mut self, provider: Arc<dyn CuratedContextProvider>) -> Self {
        self.curated_context_provider = Some(provider);
        self
    }

    /// Replace the immutable per-layer context limits for every turn.
    #[must_use]
    pub fn turn_context_budgets(mut self, budgets: TurnContextBudgets) -> Self {
        self.turn_context_budgets = budgets;
        self
    }

    /// Enable bus-based tool approval (opt-in).
    #[must_use]
    pub fn enable_approval(mut self) -> Self {
        self.approval_enabled = true;
        self
    }

    /// Set static approval rules. Auto-approve/auto-reject matching tools
    /// before falling back to bus approval.
    #[must_use]
    pub fn approval_rules(mut self, rules: Vec<sylvander_agent::approval::ApprovalRule>) -> Self {
        self.approval_enabled = true;
        self.approval_rules = rules;
        self
    }

    /// Enable durable exact-request approvals. Without this explicit store,
    /// the Agent advertises only one-shot and session scopes.
    #[must_use]
    pub fn approval_store(mut self, path: impl Into<PathBuf>) -> Self {
        self.approval_enabled = true;
        self.approval_store_path = Some(path.into());
        self
    }

    #[must_use]
    pub fn workspace_journal(mut self, path: impl Into<PathBuf>) -> Self {
        self.workspace_journal_path = Some(path.into());
        self
    }

    /// Inject the immutable execution environments selected by Runtime.
    #[must_use]
    pub(crate) fn execution_service(mut self, service: RuntimeExecutionService) -> Self {
        self.execution_service = Some(service);
        self
    }

    pub fn override_compression(mut self, pipeline: CompressionPipeline) -> Self {
        self.compression_overrides = Some(pipeline);
        self
    }

    /// Build the [`AgentRun`] without exposing its session issuer.
    pub fn build(self) -> Result<AgentRun, AgentRunError> {
        self.build_with_session_issuer().map(|(run, _)| run)
    }

    /// Build a run and return the runtime-owned issuer for authenticated
    /// session admission. Keep the issuer at the trusted service boundary.
    pub fn build_with_session_issuer(
        self,
    ) -> Result<(AgentRun, AgentSessionIssuer), AgentRunError> {
        let execution_service = self
            .execution_service
            .ok_or_else(|| AgentRunError::Build("Runtime execution service is required".into()))?;
        let id = self.spec.id.clone();
        let bus = self
            .bus
            .ok_or_else(|| AgentRunError::Build("bus is required".into()))?;

        let approval_memory =
            ApprovalMemory::load(self.approval_store_path.clone()).map_err(AgentRunError::Build)?;
        let (memory, memory_source) = match self.memory {
            Some(store) => (Some(store), MemorySource::RuntimeInjected),
            None => (None, MemorySource::None),
        };

        if self.model.reference.provider != self.spec.model.provider
            || self.model.reference.model != self.spec.model.model_name
        {
            return Err(AgentRunError::Build(
                "provider model does not match the Agent specification".into(),
            ));
        }
        let model_info = self.model.clone();
        let primary_selection = sylvander_api::ModelSelection {
            provider_id: self.model.reference.provider.clone(),
            model_id: self.model.reference.model.clone(),
        };
        let mut catalog = self
            .available_provider_models
            .iter()
            .map(|exact| {
                (
                    sylvander_api::ModelSelection {
                        provider_id: exact.reference.provider.clone(),
                        model_id: exact.reference.model.clone(),
                    },
                    exact.clone(),
                    Some(exact.clone()),
                )
            })
            .collect::<Vec<_>>();
        catalog.push((
            primary_selection.clone(),
            model_info.clone(),
            Some(self.model.clone()),
        ));
        let available_models = catalog
            .into_iter()
            .map(|(selection, shadow, exact)| {
                let model = RuntimeModel {
                    lifecycle: self
                        .qualified_model_lifecycles
                        .get(&selection)
                        .cloned()
                        .unwrap_or_default(),
                    pricing: self.qualified_model_pricing.get(&selection).copied(),
                    selection: selection.clone(),
                    shadow,
                    exact,
                };
                (selection, model)
            })
            .collect();
        let runtime_models = RuntimeModels {
            available: available_models,
            current: primary_selection,
            reasoning_effort: sylvander_api::ReasoningEffort::Off,
        };
        let runtime_permissions = sylvander_api::PermissionProfile {
            file_access: sylvander_api::FileAccess::WorkspaceWrite,
            network_access: sylvander_api::NetworkAccess::Denied,
            approval_policy: if self.approval_enabled {
                sylvander_api::ApprovalPolicy::Ask
            } else {
                sylvander_api::ApprovalPolicy::Allow
            },
        };

        let mut loop_builder = AgentLoop::builder()
            .max_iterations(self.spec.behavior.max_iterations)
            .max_retries(self.spec.behavior.max_retries);
        if let Some(pipeline) = self.compression_overrides {
            loop_builder = loop_builder.compression_pipeline(pipeline);
        }
        let loop_config = loop_builder.build();
        let system_prompt = (!self.spec.persona.system_prompt.is_empty())
            .then(|| self.spec.persona.system_prompt.clone());
        let tools = self.tool_overrides.unwrap_or_default();
        let invocation_gateway = self
            .invocation_gateway
            .unwrap_or_else(|| RegistryBoundToolGateway::new(tools.invocation_descriptors()));

        let workspace_journal = self
            .workspace_journal_path
            .map(|path| Arc::new(WorkspaceJournal::new(path)));
        let session_authority = Arc::new(SessionAuthorityMarker);
        let issuer = AgentSessionIssuer {
            authority: session_authority.clone(),
        };
        let run = AgentRun {
            inner: Arc::new(AgentRunInner {
                id,
                spec: self.spec,
                loop_config,
                model_provider: self.router,
                system_prompt,
                tools,
                invocation_gateway,
                session_tool_surfaces: RwLock::new(HashMap::new()),
                runtime_models: RwLock::new(runtime_models),
                runtime_permissions: RwLock::new(runtime_permissions),
                prompt_resolver: self.prompt_resolver,
                user_profile_provider: self.user_profile_provider,
                curated_context_provider: self.curated_context_provider,
                turn_context_budgets: self.turn_context_budgets,
                turn_context_manifests: RwLock::new(HashMap::new()),
                context_usage: RwLock::new(HashMap::new()),
                workspace_journal,
                execution_service,
                artifact_service: self.artifact_service,
                skill_features: std::sync::RwLock::new(Vec::new()),
                bus,
                observability: self.observability,
                sessions: RwLock::new(HashMap::new()),
                authenticated_sessions: RwLock::new(HashSet::new()),
                authenticated_session_authority_active: AtomicBool::new(false),
                session_authority,
                session_store: self.session_store,
                workflow_store: self.workflow_store,
                memory,
                memory_source,
                approval_enabled: self.approval_enabled,
                approval_rules: self.approval_rules,
                pending_approvals: Arc::new(Mutex::new(HashMap::new())),
                approval_memory: Arc::new(Mutex::new(approval_memory)),
                pending_answers: Arc::new(Mutex::new(HashMap::new())),
                pending_plans: Arc::new(Mutex::new(HashMap::new())),
                session_locks: Mutex::new(HashMap::new()),
                active_turns: Mutex::new(HashMap::new()),
                turn_snapshots: RwLock::new(HashMap::new()),
            }),
        };
        Ok((run, issuer))
    }
}
