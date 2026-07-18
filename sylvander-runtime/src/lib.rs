//! # sylvander-runtime
//!
//! System runtime — the bootstrap and orchestration layer above the
//! agent engine.
//!
//! The runtime:
//! 1. Boots the system (creates bus, engine, session store)
//! 2. Spawns agents from configuration
//! 3. Loads persistent sessions
//! 4. Starts protocol channels (TUI, Telegram, ...)
//!
//! # Architecture
//!
//! ```text
//! Channel (TUI / Telegram / ...)
//!       │  normalize external messages → BusMessage
//!       ▼
//! ┌──────────────────┐
//! │  sylvander-runtime│  durable boot / session lifecycle / shutdown
//! ├──────────────────┤
//! │  sylvander-agent  │  AgentRunEngine / AgentRun / AgentLoop
//! └──────────────────┘
//! ```

mod agent_admin;
#[cfg(test)]
#[path = "../tests/unit/agent_admin_runtime_v3.rs"]
mod agent_admin_runtime_v3_tests;
/// Versioned Agent definitions and active-revision lookup.
pub mod agent_registry;
#[allow(dead_code)] // versioned contract staged before SQL composition wiring
mod agent_registry_snapshot_v3;
#[cfg(test)]
#[path = "../tests/unit/agent_registry_snapshot_v3_contract.rs"]
mod agent_registry_snapshot_v3_tests;
mod boundary;
mod capability_runtime;
/// Target-aware local and remote coding-session isolation.
pub mod coding_worktree;
/// Builds configured Agent revisions, prompt layers, providers, and tools.
pub mod composition;
/// Latest-version server configuration and secret-reference contracts.
pub mod config;
/// Durable, content-safe Provider and Channel credential operation audit.
pub mod credential_audit;
#[allow(dead_code)] // internal API consumed by credential administration batches
mod credential_registry;
#[cfg(test)]
#[path = "../tests/unit/credential_registry.rs"]
mod credential_registry_tests;
/// Content-safe runtime evidence, feedback, and authorization records.
pub mod evidence;
/// Workspace target selection and execution policy composition.
pub mod execution;
/// Isolated local Git worktree lease lifecycle for coding sessions.
pub mod git_worktree;
mod guardian_curation;
mod guardian_runtime;
#[allow(dead_code)] // runtime ownership/config wiring follows this isolated policy adapter
mod identity_binding_service;
#[cfg(test)]
#[path = "../tests/unit/identity_binding_service.rs"]
mod identity_binding_service_tests;
mod memory_maintenance;
#[allow(dead_code)] // internal API consumed by model routing/admin batches
mod model_registry;
#[cfg(test)]
#[path = "../tests/unit/model_registry.rs"]
mod model_registry_tests;
/// Stable user mapping for authenticated transport principals.
pub mod principal_binding;
#[cfg(test)]
#[path = "../tests/unit/principal_binding.rs"]
mod principal_binding_tests;
/// Controlled synchronization of provider model catalogs into the registry.
pub mod provider_catalog_sync;
#[allow(dead_code)] // internal API consumed by provider routing/admin batches
mod provider_registry;
#[cfg(test)]
#[path = "../tests/unit/provider_registry.rs"]
mod provider_registry_tests;
#[allow(dead_code)] // production handler wiring follows the audited transport seam
mod registry_admin;
#[allow(dead_code)] // pure bootstrap plan; executor wiring follows registry snapshots
mod registry_bootstrap;
#[cfg(test)]
#[path = "../tests/unit/registry_bootstrap.rs"]
mod registry_bootstrap_tests;
#[allow(dead_code)] // versioned composition is wired into Agent construction next
mod registry_composition_v3;
#[cfg(test)]
#[path = "../tests/unit/registry_composition_v3.rs"]
mod registry_composition_v3_tests;
#[allow(dead_code)] // consumed by the staged registry mutation batches
mod registry_domain;
#[cfg(test)]
#[path = "../tests/unit/registry_domain.rs"]
mod registry_domain_tests;
/// Durable executor-backed Git worktree leases for remote coding sessions.
pub mod remote_git_worktree;
#[allow(dead_code)] // wired by registry-backed composition after snapshot resolution
mod request_scoped_provider;
#[cfg(test)]
#[path = "../tests/unit/runtime_external_provider.rs"]
mod runtime_external_provider_tests;
pub use request_scoped_provider::{
    ExternalSecretLease, ExternalSecretLeaseError, ExternalSecretLeaseFuture,
    MAX_EXTERNAL_SECRET_LEASE_SECONDS, RenewableExternalSecretProvider, SecretLeaseMetadata,
};
/// Evidence-backed, human-gated self-change experiments.
pub mod self_change;
#[allow(dead_code)] // Runtime-owned profile dispatch is integrated in the next bounded batch
mod user_profile_store;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use sylvander_agent::bus::{
    BusDiagnostics, BusMessage, InProcessMessageBus, MessageBus, Recipient, SubscriptionFilter,
};
use sylvander_agent::engine::{AgentRunEngine, RevisionedAgentRunProvider};
use sylvander_agent::mcp_stdio::McpResultArtifactSink;
#[cfg(test)]
use sylvander_agent::run::AgentRun;
use sylvander_agent::session::SessionMetadata;
use sylvander_agent::session_store::{
    SESSION_SCHEMA_OBJECT_NAMES, SessionLifetime, SessionStore, SqliteSessionStore, StoredSession,
};
#[cfg(test)]
use sylvander_agent::spec::AgentSpec;
use sylvander_agent::spec::{AgentId, SessionId};
#[cfg(test)]
use sylvander_agent::tools::InMemoryMemoryStore;
use sylvander_agent::tools::{
    HttpMemoryIntegrityAnchor, HttpMemoryIntegrityAnchorConfig, MemoryIntegrityConfig, MemoryStore,
    SqliteMemoryStore,
};
use sylvander_channel::{
    AuthenticatedTransportIdentity, Channel, ChannelContext, ChannelReadiness,
};
#[cfg(test)]
use sylvander_llm_anthropic::{AnthropicProvider, api::client::AnthropicClient};
#[cfg(test)]
use sylvander_llm_core::{
    ModelCapabilities as ProviderModelCapabilities, ModelInfo as ProviderModelInfo, ModelRef,
};
use sylvander_protocol::{
    AgentAdminError, AgentAdminErrorCode, AgentAdminRequest, AgentAdminResponse, AgentAdminResult,
    AgentDescriptor, IdentityBindingCapabilities, IdentityBindingError, IdentityBindingErrorCode,
    IdentityBindingRequest, IdentityBindingResponse, MemoryConfirmationErrorCode,
    MemoryConfirmationRequest, MemoryConfirmationResponse, MemoryConfirmationValidationError,
    ModelSelection, RegistryAdminError, RegistryAdminErrorCode, RegistryAdminRequest,
    RegistryAdminResponse, RunFeedback, SessionConfigOverrides, SessionConfigState,
    SessionConfigUpdateRequest, SessionCreateRequest, SessionEffectiveConfig,
    SessionRevisionPinError, USER_PROFILE_PROTOCOL_VERSION, UserId, UserProfileAction,
    UserProfileCapabilities, UserProfileError, UserProfileErrorCode, UserProfileOperation,
    UserProfileRequest, UserProfileResponse,
};

use crate::agent_admin::{
    AgentAdminDispatch, AgentAdminService, is_agent_administrator, map_registry_error,
    redact_revision,
};
use crate::agent_registry_snapshot_v3::{AgentSnapshotSelectionV3, AgentSnapshotV3Error};
#[cfg(test)]
use crate::composition::default_tools;
use crate::composition::{
    ConfiguredAgent, build_registry_agent_versioned_with_resolver, resolve_session_config,
};
use crate::config::{
    MemoryIntegrityBackend, SecretResolver, ServerConfig, ServerMode, SystemSecretResolver,
};
use crate::credential_audit::CredentialOperationAuditLedger;
use crate::credential_registry::CredentialSecretResolver;
use crate::evidence::{
    AdministrationAudit, AuthorizationDenial, EvidenceArtifactSink, EvidenceEncryption,
    EvidenceGovernance, EvidenceRecorder, EvidenceStore,
};
use crate::guardian_runtime::{
    GuardianRuntime, GuardianRuntimeError, GuardianRuntimeSettings, WorkerToolGatewayFactory,
};
use crate::identity_binding_service::{
    IdentityBindingService, IdentityIngress, TrustedIdentityIssuer,
};
use crate::memory_maintenance::{
    MemoryMaintenanceTask, RuntimeMemoryMaintenancePolicy, catch_up as memory_maintenance_catch_up,
};
use crate::principal_binding::{PrincipalBindingError, PrincipalBindingStore, PrincipalDigestKey};
use crate::registry_admin::{CredentialRegistryMutationService, RegistryAdminService};
use crate::user_profile_store::{UserProfileStore, UserProfileStoreError};
use agent_registry::{AgentRegistry, REGISTRY_SCHEMA_OBJECT_NAMES};
use boundary::BoundaryGuard;

fn bind_effective_workspace(effective: &mut SessionEffectiveConfig, workspace: &std::path::Path) {
    let canonical_mount = if let Some(binding) = effective.user_workspace.as_mut() {
        binding.path = workspace.to_path_buf();
        Some("task")
    } else if let Some(binding) = effective.agent_workspace.as_mut() {
        binding.path = workspace.to_path_buf();
        Some("agent")
    } else {
        None
    };
    if let Some(mount) = canonical_mount.and_then(|reference| {
        effective
            .workspace_mounts
            .iter_mut()
            .find(|mount| mount.reference == reference)
    }) {
        mount.binding.path = workspace.to_path_buf();
    }
}

fn ensure_remote_mutation_mounts_are_transactional(
    effective: &SessionEffectiveConfig,
    worktrees: &coding_worktree::CodingWorktreeService,
) -> Result<(), String> {
    let canonical_mount = if effective.user_workspace.is_some() {
        Some("task")
    } else if effective.agent_workspace.is_some() {
        Some("agent")
    } else {
        None
    };
    ensure_remote_mutation_mounts_are_transactional_with(
        &effective.workspace_mounts,
        canonical_mount,
        |target| worktrees.is_remote_target(target),
    )
}

fn ensure_remote_mutation_mounts_are_transactional_with(
    mounts: &[sylvander_protocol::SessionWorkspaceMount],
    canonical_mount: Option<&str>,
    is_remote: impl Fn(&str) -> bool,
) -> Result<(), String> {
    for mount in mounts {
        let can_mutate =
            !mount.binding.read_only && (mount.capabilities.write || mount.capabilities.command);
        if can_mutate
            && is_remote(&mount.binding.execution_target)
            && canonical_mount != Some(mount.reference.as_str())
        {
            return Err(format!(
                "writable remote workspace mount `@{}` requires its own worktree transaction",
                mount.reference
            ));
        }
    }
    Ok(())
}

fn ensure_workspace_update_is_static(
    session: &StoredSession,
    overrides: &SessionConfigOverrides,
) -> Result<(), String> {
    if session.config_overrides.user_workspace != overrides.user_workspace
        || session.config_overrides.execution_target != overrides.execution_target
    {
        return Err(
            "workspace and execution target cannot change after session creation; create a new session"
                .into(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SystemConfig
// ---------------------------------------------------------------------------

/// Test-only in-memory bootstrap configuration.
///
/// Production always boots from validated [`ServerConfig`] so sessions,
/// evidence, memory, credentials, and Guardian state remain durable.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct SystemConfig {
    /// Human-readable system name.
    pub name: String,
    /// Agents to spawn at boot.
    pub agents: Vec<AgentSpec>,
    /// Pre-defined persistent sessions to load/create at boot.
    pub sessions: Vec<StoredSession>,
}

/// Runtime dependencies for live Provider credential acquisition.
///
/// `resolver` supports registry preflight and non-model execution credentials.
/// `lease_provider` owns acquire/renew against the external secret service.
/// Neither dependency is formatted by this wrapper.
#[derive(Clone)]
pub struct ProviderCredentialSources {
    resolver: Arc<dyn CredentialSecretResolver>,
    lease_provider: Arc<dyn RenewableExternalSecretProvider>,
}

impl ProviderCredentialSources {
    /// Construct an injectable Provider credential boundary.
    pub fn new(
        resolver: Arc<dyn SecretResolver>,
        lease_provider: Arc<dyn RenewableExternalSecretProvider>,
    ) -> Self {
        Self {
            resolver: Arc::new(SecretResolverBridge(resolver)),
            lease_provider,
        }
    }
}

impl fmt::Debug for ProviderCredentialSources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCredentialSources")
            .field("resolver", &"[REDACTED]")
            .field("lease_provider", &"[REDACTED]")
            .finish()
    }
}

struct SecretResolverBridge(Arc<dyn SecretResolver>);

impl CredentialSecretResolver for SecretResolverBridge {
    fn resolve_credential(&self, reference: &config::SecretRef) -> Result<config::SecretValue, ()> {
        self.0.resolve(reference).map_err(|_| ())
    }
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// The system runtime — top-level orchestrator.
pub struct Runtime {
    /// The agent lifecycle engine.
    engine: Arc<AgentRunEngine>,
    /// Session persistence backend.
    pub session_store: Arc<dyn SessionStore>,
    /// Runtime-owned long-term memory shared by every Agent revision.
    pub memory_store: Arc<dyn MemoryStore>,
    /// Shared message bus.
    bus: Arc<dyn MessageBus>,
    /// Fully configured runs retained for protocol control operations.
    configured_agents: HashMap<AgentId, ConfiguredAgent>,
    revision_provider: Option<Arc<RuntimeRevisionProvider>>,
    ui_service: Arc<RuntimeUiService>,
    evidence: Option<EvidenceRecorder>,
    credential_audit: Option<Arc<CredentialOperationAuditLedger>>,
    guardian: Option<Arc<GuardianRuntime>>,
    memory_maintenance: Option<MemoryMaintenanceTask>,
    channels: tokio::sync::Mutex<Vec<ChannelTask>>,
    channel_exit_tx: tokio::sync::mpsc::UnboundedSender<String>,
    channel_exits: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>,
}

struct ChannelTask {
    instance_id: String,
    task: JoinHandle<()>,
    lifecycle: ChannelReadiness,
    health: Arc<std::sync::RwLock<ChannelHealth>>,
}

/// One configured channel instance owned by the Runtime supervisor.
#[derive(Clone)]
pub struct ChannelRegistration {
    pub instance_id: String,
    pub channel: Arc<dyn Channel>,
    pub restart: ChannelRestartPolicy,
    pub session_defaults: SessionConfigOverrides,
}

impl ChannelRegistration {
    #[must_use]
    pub fn new(instance_id: impl Into<String>, channel: Arc<dyn Channel>) -> Self {
        Self {
            instance_id: instance_id.into(),
            channel,
            restart: ChannelRestartPolicy::default(),
            session_defaults: SessionConfigOverrides::default(),
        }
    }

    #[must_use]
    pub fn with_restart_policy(mut self, restart: ChannelRestartPolicy) -> Self {
        self.restart = restart;
        self
    }

    #[must_use]
    pub fn with_session_defaults(mut self, defaults: SessionConfigOverrides) -> Self {
        self.session_defaults = defaults;
        self
    }
}

impl fmt::Debug for ChannelRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChannelRegistration")
            .field("instance_id", &self.instance_id)
            .field("kind", &self.channel.name())
            .field("restart", &self.restart)
            .field("session_defaults", &self.session_defaults)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelRestartPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for ChannelRestartPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelStatus {
    Starting,
    Ready,
    Restarting,
    Failed,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelHealth {
    pub instance_id: String,
    pub kind: String,
    pub status: ChannelStatus,
    pub restart_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeOperationalSnapshot {
    pub ready: bool,
    pub agent_count: usize,
    pub persistent_session_count: usize,
    pub channels: Vec<ChannelHealth>,
    pub bus: BusDiagnostics,
    pub evidence: Option<evidence::EvidenceCounts>,
    pub health_issues: Vec<RuntimeHealthIssue>,
}

/// Content-safe durable subsystem failures that make Runtime unready.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeHealthIssue {
    EvidenceRecorder,
    GuardianSupervisor,
}

struct RuntimeUiService {
    engine: Arc<AgentRunEngine>,
    bus: Arc<dyn MessageBus>,
    sessions: Arc<dyn SessionStore>,
    agents: HashMap<AgentId, ConfiguredAgent>,
    agent_registry: Option<AgentRegistry>,
    revision_provider: Option<Arc<RuntimeRevisionProvider>>,
    credential_resolver: Option<Arc<dyn CredentialSecretResolver>>,
    credential_audit: Option<Arc<CredentialOperationAuditLedger>>,
    evidence: Option<EvidenceStore>,
    evidence_run_id: Option<String>,
    guardian: Option<Arc<GuardianRuntime>>,
    identity_bindings: Option<Arc<IdentityBindingService>>,
    user_profiles: Option<UserProfileStore>,
    worktrees: Option<Arc<coding_worktree::CodingWorktreeService>>,
    boundary: BoundaryGuard,
}

struct RuntimeRevisionProvider {
    config: ServerConfig,
    registry: AgentRegistry,
    bus: Arc<dyn MessageBus>,
    sessions: Arc<dyn SessionStore>,
    memory: Arc<dyn MemoryStore>,
    user_profiles: Arc<dyn sylvander_agent::user_profile_provider::UserProfileProvider>,
    credential_resolver: Arc<dyn CredentialSecretResolver>,
    external_secret_provider: Option<Arc<dyn RenewableExternalSecretProvider>>,
    credential_audit: Arc<CredentialOperationAuditLedger>,
    result_artifacts: Option<Arc<dyn McpResultArtifactSink>>,
    tool_gateway_factory: WorkerToolGatewayFactory,
    configured: RwLock<HashMap<(AgentId, u64), ConfiguredAgent>>,
}

impl RuntimeRevisionProvider {
    async fn compose_revision(
        &self,
        agent_id: &AgentId,
        revision: u64,
    ) -> Result<ConfiguredAgent, RuntimeError> {
        let snapshot = self
            .registry
            .resolve_registry_composition_versioned(agent_id, revision)
            .await
            .map_err(|error| RuntimeError::Composition(error.to_string()))?;
        build_registry_agent_versioned_with_resolver(
            &self.config,
            snapshot,
            self.registry.clone(),
            self.bus.clone(),
            self.sessions.clone(),
            self.memory.clone(),
            Some(self.user_profiles.clone()),
            self.credential_resolver.clone(),
            self.external_secret_provider.clone(),
            self.credential_audit.clone(),
            self.result_artifacts.clone(),
            Some(self.tool_gateway_factory.clone()),
        )
        .await
        .map_err(|error| RuntimeError::Composition(error.to_string()))
    }

    async fn configured_revision(
        &self,
        agent_id: &AgentId,
        revision: u64,
    ) -> Result<ConfiguredAgent, RuntimeError> {
        let key = (agent_id.clone(), revision);
        if let Some(configured) = self.configured.read().await.get(&key).cloned() {
            return Ok(configured);
        }
        let configured = self.compose_revision(agent_id, revision).await?;
        let mut cache = self.configured.write().await;
        Ok(cache
            .entry(key)
            .or_insert_with(|| configured.clone())
            .clone())
    }

    async fn revalidate_revision(
        &self,
        agent_id: &AgentId,
        revision: u64,
    ) -> Result<ConfiguredAgent, RuntimeError> {
        let configured = self.compose_revision(agent_id, revision).await?;
        self.configured
            .write()
            .await
            .insert((agent_id.clone(), revision), configured.clone());
        Ok(configured)
    }

    async fn active_agent(&self, agent_id: &AgentId) -> Result<ConfiguredAgent, RuntimeError> {
        let active = self
            .registry
            .load_active(agent_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .ok_or_else(|| RuntimeError::Config(format!("unknown Agent {agent_id}")))?;
        self.configured_revision(agent_id, active.definition.revision)
            .await
    }

    async fn bound_revision(
        &self,
        agent_id: &AgentId,
        session_id: &SessionId,
    ) -> Result<u64, RuntimeError> {
        let session = self
            .sessions
            .get(session_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .ok_or_else(|| RuntimeError::Config(format!("session {session_id} is not bound")))?;
        self.bound_stored_revision(agent_id, &session).await
    }

    async fn bound_stored_revision(
        &self,
        agent_id: &AgentId,
        session: &StoredSession,
    ) -> Result<u64, RuntimeError> {
        let effective = session.effective_config.as_ref().ok_or_else(|| {
            RuntimeError::SessionBinding(SessionBindingError::UnresolvedPins(session.id.clone()))
        })?;
        if &effective.agent_id != agent_id {
            return Err(RuntimeError::Config(format!(
                "session {} is bound to Agent {}, not {agent_id}",
                session.id, effective.agent_id
            )));
        }
        let configured = self
            .configured_revision(&effective.agent_id, effective.agent_revision)
            .await?;
        let closure = close_session_revision_pins(&self.registry, session, &configured).await?;
        if closure.changed {
            return Err(RuntimeError::SessionBinding(
                SessionBindingError::UnresolvedPins(session.id.clone()),
            ));
        }
        Ok(closure.effective.agent_revision)
    }
}

fn active_snapshot_selection(
    definition: &crate::config::AgentDefinitionConfig,
) -> AgentSnapshotSelectionV3 {
    let provider_id = &definition.spec.model.provider;
    let allowed_models = definition
        .spec
        .model
        .allowed_models
        .iter()
        .cloned()
        .collect();
    AgentSnapshotSelectionV3 {
        agent_id: definition.spec.id.to_string(),
        agent_revision: definition.revision,
        default_model: ModelSelection {
            provider_id: provider_id.clone(),
            model_id: definition.spec.model.model_name.clone(),
        },
        allowed_models,
    }
}

struct SessionPinClosure {
    effective: SessionEffectiveConfig,
    changed: bool,
}

async fn close_session_revision_pins(
    registry: &AgentRegistry,
    session: &StoredSession,
    _active_agent: &ConfiguredAgent,
) -> Result<SessionPinClosure, SessionBindingError> {
    let [member] = session.agents.as_slice() else {
        return Err(SessionBindingError::InvalidMembership(session.id.clone()));
    };
    let effective = if let Some(effective) = &session.effective_config {
        if member != &effective.agent_id {
            return Err(SessionBindingError::AgentMismatch {
                session_id: session.id.clone(),
                expected: member.clone(),
                actual: effective.agent_id.clone(),
            });
        }
        effective.clone()
    } else {
        return Err(SessionBindingError::UnresolvedPins(session.id.clone()));
    };
    let snapshot = registry
        .load_agent_snapshot_versioned(&effective.agent_id.0, effective.agent_revision)
        .await
        .map_err(|_| SessionBindingError::Snapshot)?
        .ok_or_else(|| SessionBindingError::MissingSnapshot {
            agent_id: effective.agent_id.clone(),
            revision: effective.agent_revision,
        })?;
    snapshot
        .validate()
        .map_err(|_| SessionBindingError::Snapshot)?;
    let pinned_agent = registry
        .load(&effective.agent_id, effective.agent_revision)
        .await
        .map_err(|_| SessionBindingError::Registry)?
        .ok_or_else(|| SessionBindingError::MissingAgentRevision {
            agent_id: effective.agent_id.clone(),
            revision: effective.agent_revision,
        })?;
    let configured_default = ModelSelection {
        provider_id: pinned_agent.definition.spec.model.provider.clone(),
        model_id: pinned_agent.definition.spec.model.model_name.clone(),
    };
    if snapshot.default_model != configured_default {
        return Err(SessionBindingError::Snapshot);
    }
    let provider_revision = snapshot
        .providers
        .get(&effective.provider_id)
        .copied()
        .ok_or_else(|| SessionBindingError::MissingProvider {
            provider_id: effective.provider_id.clone(),
        })?;
    let selection = ModelSelection {
        provider_id: effective.provider_id.clone(),
        model_id: effective.model_id.clone(),
    };
    let model = snapshot
        .models
        .iter()
        .find(|model| model.model == selection)
        .ok_or_else(|| SessionBindingError::MissingModel {
            provider_id: effective.provider_id.clone(),
            model_id: effective.model_id.clone(),
        })?;
    if effective.provider_revision != provider_revision {
        return Err(SessionBindingError::ProviderRevisionMismatch {
            expected: provider_revision,
            actual: effective.provider_revision,
        });
    }
    if effective.model_revision != model.revision {
        return Err(SessionBindingError::ModelRevisionMismatch {
            expected: model.revision,
            actual: effective.model_revision,
        });
    }
    effective
        .require_revision_pins()
        .map_err(SessionBindingError::InvalidPins)?;
    Ok(SessionPinClosure {
        effective,
        changed: false,
    })
}

#[async_trait::async_trait]
impl RevisionedAgentRunProvider for RuntimeRevisionProvider {
    async fn revision_for_session(
        &self,
        agent_id: &AgentId,
        session_id: &SessionId,
    ) -> Result<u64, String> {
        self.bound_revision(agent_id, session_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn run_for_revision(
        &self,
        agent_id: &AgentId,
        revision: u64,
    ) -> Result<sylvander_agent::run::AgentRun, String> {
        self.configured_revision(agent_id, revision)
            .await
            .map(|configured| configured.run)
            .map_err(|error| error.to_string())
    }
}

#[async_trait::async_trait]
impl sylvander_channel::UiService for RuntimeUiService {
    async fn reject_authentication(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        failure: sylvander_protocol::AuthenticationFailure,
    ) -> sylvander_protocol::BoundaryError {
        let operation = failure.operation();
        let error = if boundary.principal.is_some() {
            sylvander_protocol::BoundaryError {
                code: sylvander_protocol::BoundaryErrorCode::InvalidScope,
                operation: operation.into(),
                request_id: boundary.request_id.clone(),
                message: "authentication failure requires an unauthenticated boundary".into(),
                retry_after_ms: None,
            }
        } else {
            match self
                .boundary
                .check_authentication_failure(boundary, operation)
                .await
            {
                Ok(()) => sylvander_protocol::BoundaryError::unauthenticated(boundary, operation),
                Err(error) => error,
            }
        };
        if let Err(audit_error) = self
            .record_boundary_denial(boundary, operation, None, &error)
            .await
        {
            warn!(%audit_error, request_id = %boundary.request_id, "failed to persist authentication denial");
        }
        error
    }

    async fn authorize_message(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        message: &sylvander_protocol::UiClientMessage,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        let result = async {
            self.boundary
                .check(boundary, message, ui_operation(message))
                .await?;
            self.authorize_message_inner(boundary, message).await
        }
        .await;
        if let Err(error) = &result
            && let Err(audit_error) = self.record_denial(boundary, message, error).await
        {
            warn!(%audit_error, request_id = %boundary.request_id, "failed to persist authorization denial");
        }
        result
    }

    async fn discover_agents(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
    ) -> Result<Vec<AgentDescriptor>, sylvander_protocol::BoundaryError> {
        require_principal(boundary, "discover_agents")?;
        let mut agents = Vec::new();
        for agent_id in self.agents.keys() {
            if !self
                .current_agent_access_allowed(agent_id, boundary, "discover_agents")
                .await?
            {
                continue;
            }
            let agent = self
                .active_agent(agent_id, boundary, "discover_agents")
                .await?;
            let runtime_models = agent.run.runtime_model_info().await;
            agents.push(AgentDescriptor {
                id: agent.spec.id.clone(),
                revision: agent.definition.revision,
                name: agent.spec.name.clone(),
                provider_id: agent.spec.model.provider.clone(),
                default_model_id: agent.spec.model.model_name.clone(),
                models: runtime_models.models,
                default_prompt_profile: agent.definition.default_prompt_profile.clone(),
                agent_workspace: agent.definition.agent_workspace.as_ref().map(|workspace| {
                    sylvander_protocol::SessionWorkspaceBinding {
                        execution_target: workspace.execution_target.clone(),
                        path: workspace.path.clone().into(),
                        read_only: workspace.read_only,
                        instruction_focus: workspace.instruction_focus.clone().map(Into::into),
                    }
                }),
            });
        }
        agents.sort_by(|left, right| left.id.0.cmp(&right.id.0));
        Ok(agents)
    }

    async fn list_sessions(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
    ) -> Result<Vec<sylvander_protocol::UiSessionInfo>, sylvander_protocol::BoundaryError> {
        require_principal(boundary, "list_sessions")?;
        let user_id = self.effective_user_id(boundary, "list_sessions").await?;
        let sessions = self
            .sessions
            .list_persistent()
            .await
            .map_err(|error| boundary_failure(boundary, "list_sessions", error.to_string()))?;
        let now = sylvander_agent::session::now_secs();
        let mut visible = Vec::new();
        for session in sessions {
            if session.metadata.user_id != user_id.0 && !privileged_principal(boundary) {
                continue;
            }
            let mut allowed = true;
            for agent_id in &session.agents {
                if !self
                    .current_agent_access_allowed(agent_id, boundary, "list_sessions")
                    .await?
                {
                    allowed = false;
                    break;
                }
            }
            if !allowed || session.agents.is_empty() {
                continue;
            }
            visible.push(sylvander_protocol::UiSessionInfo {
                id: session.id.0,
                label: if session.name.is_empty() {
                    "untitled session".into()
                } else {
                    session.name
                },
                workspace: session.metadata.workspace.display().to_string(),
                last_seen_secs: u64::try_from(now.saturating_sub(session.updated_at)).unwrap_or(0),
            });
        }
        Ok(visible)
    }

    async fn create_session(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: SessionCreateRequest,
    ) -> Result<SessionConfigState, sylvander_protocol::BoundaryError> {
        self.create_session_with_metadata(boundary, request, BTreeMap::new())
            .await
    }

    async fn session_config(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<SessionConfigState, sylvander_protocol::BoundaryError> {
        let session = self
            .owned_session(boundary, session_id, "get_session_config")
            .await?;
        let effective = session.effective_config.ok_or_else(|| {
            boundary_failure(
                boundary,
                "get_session_config",
                format!("session {session_id} has no effective configuration"),
            )
        })?;
        Ok(SessionConfigState {
            session_id: session.id,
            revision: session.config_revision,
            overrides: session.config_overrides,
            effective,
        })
    }

    async fn update_session_config(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: SessionConfigUpdateRequest,
    ) -> Result<SessionConfigState, sylvander_protocol::BoundaryError> {
        let session = self
            .owned_session(boundary, &request.session_id, "update_session_config")
            .await?;
        ensure_workspace_update_is_static(&session, &request.overrides)
            .map_err(|error| boundary_failure(boundary, "update_session_config", error))?;
        let agent = session
            .agents
            .iter()
            .find_map(|id| self.agents.get(id))
            .cloned()
            .ok_or_else(|| {
                boundary_failure(
                    boundary,
                    "update_session_config",
                    format!("session {} has no configured Agent", request.session_id),
                )
            })?;
        let agent = self
            .bind_session_revision(boundary, &session, agent, "update_session_config")
            .await?;
        let mut effective = resolve_session_config(&agent, &request.overrides, None, None)
            .map_err(|error| {
                boundary_failure(boundary, "update_session_config", error.to_string())
            })?;
        bind_effective_workspace(&mut effective, &session.metadata.workspace);
        let revision = self
            .sessions
            .update_config(
                &request.session_id,
                request.expected_revision,
                request.overrides.clone(),
                effective.clone(),
            )
            .await
            .map_err(|error| {
                boundary_failure(boundary, "update_session_config", error.to_string())
            })?;
        Ok(SessionConfigState {
            session_id: request.session_id,
            revision,
            overrides: request.overrides,
            effective,
        })
    }

    async fn submit_feedback(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        feedback: RunFeedback,
    ) -> Result<String, sylvander_protocol::BoundaryError> {
        let principal = require_principal(boundary, "submit_feedback")?;
        if !feedback.target.is_well_formed() {
            return Err(boundary_failure(
                boundary,
                "submit_feedback",
                "feedback target is not a server-issued SHA-256 handle",
            ));
        }
        if feedback.note.as_ref().is_some_and(|note| note.len() > 4096)
            || feedback
                .correction
                .as_ref()
                .is_some_and(|correction| correction.len() > 4096)
        {
            return Err(boundary_failure(
                boundary,
                "submit_feedback",
                "feedback note or correction exceeds 4096 bytes",
            ));
        }
        if feedback.tags.len() > 16
            || feedback
                .tags
                .iter()
                .any(|tag| tag.is_empty() || tag.len() > 64)
        {
            return Err(boundary_failure(
                boundary,
                "submit_feedback",
                "feedback supports at most 16 non-empty tags of 64 bytes each",
            ));
        }
        if !valid_evidence_references(&feedback.artifacts)
            || !valid_evidence_references(&feedback.validations)
        {
            return Err(boundary_failure(
                boundary,
                "submit_feedback",
                "feedback evidence references are invalid",
            ));
        }
        let store = self.evidence.as_ref().ok_or_else(|| {
            boundary_failure(
                boundary,
                "submit_feedback",
                "runtime evidence capture is disabled",
            )
        })?;
        let session_id = store
            .feedback_session(feedback.target.clone())
            .await
            .map_err(|error| boundary_failure(boundary, "submit_feedback", error.to_string()))?
            .ok_or_else(|| {
                boundary_failure(
                    boundary,
                    "submit_feedback",
                    "feedback must identify one attributable session",
                )
            })?;
        let session = self
            .owned_session(boundary, &SessionId::new(session_id), "submit_feedback")
            .await?;
        let feedback_digest = serde_json::to_string(&feedback)
            .map(|encoded| format!("sha256:{}", sha256_text(&encoded)))
            .map_err(|error| boundary_failure(boundary, "submit_feedback", error.to_string()))?;
        let recorded_at = sylvander_agent::session::now_secs();
        let feedback_id = store
            .record_feedback(
                feedback,
                crate::evidence::FeedbackAttribution {
                    principal_digest: sha256_text(&principal.id.0),
                    channel_instance_id: boundary.channel_instance_id.clone(),
                    transport: boundary.transport.clone(),
                },
                recorded_at,
            )
            .await
            .map_err(|error| boundary_failure(boundary, "submit_feedback", error.to_string()))?;
        if let Some(guardian) = &self.guardian
            && let Err(error) = guardian
                .enqueue_feedback(&session, &feedback_id, &feedback_digest, recorded_at)
                .await
        {
            warn!(
                %error,
                feedback_id,
                "failed to enqueue persisted feedback for Guardian curation"
            );
        }
        Ok(feedback_id)
    }

    fn identity_binding_capabilities(&self) -> IdentityBindingCapabilities {
        self.identity_bindings
            .as_ref()
            .map_or_else(IdentityBindingCapabilities::default, |_| {
                IdentityBindingCapabilities::current()
            })
    }

    fn user_profile_capabilities(&self) -> UserProfileCapabilities {
        self.user_profiles
            .as_ref()
            .map_or_else(UserProfileCapabilities::default, |_| {
                UserProfileCapabilities::current()
            })
    }

    async fn user_profile(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: UserProfileRequest,
    ) -> UserProfileResponse {
        let message = sylvander_protocol::UiClientMessage::UserProfile { request };
        if let Err(error) = self
            .boundary
            .check(boundary, &message, "user_profile")
            .await
        {
            let code = match error.code {
                sylvander_protocol::BoundaryErrorCode::Unauthenticated => {
                    UserProfileErrorCode::Unauthenticated
                }
                sylvander_protocol::BoundaryErrorCode::RateLimited => {
                    UserProfileErrorCode::RateLimited
                }
                sylvander_protocol::BoundaryErrorCode::PayloadTooLarge => {
                    UserProfileErrorCode::InvalidRequest
                }
                _ => UserProfileErrorCode::Forbidden,
            };
            let operation = match &message {
                sylvander_protocol::UiClientMessage::UserProfile { request } => request.operation(),
                _ => unreachable!(),
            };
            return user_profile_error(operation, code, None);
        }
        let sylvander_protocol::UiClientMessage::UserProfile { request } = message else {
            unreachable!()
        };
        let operation = request.operation();
        if let Err(error) = request.validate() {
            let code = if matches!(
                error,
                sylvander_protocol::UserProfileValidationError::UnsupportedVersion
            ) {
                UserProfileErrorCode::UnsupportedVersion
            } else {
                UserProfileErrorCode::InvalidRequest
            };
            return user_profile_error(operation, code, None);
        }
        let Some(store) = &self.user_profiles else {
            return user_profile_error(operation, UserProfileErrorCode::ServiceUnavailable, None);
        };
        let owner = match self.effective_user_id(boundary, "user_profile").await {
            Ok(owner) => owner,
            Err(error) => {
                let code = match error.code {
                    sylvander_protocol::BoundaryErrorCode::Unauthenticated => {
                        UserProfileErrorCode::Unauthenticated
                    }
                    _ => UserProfileErrorCode::Forbidden,
                };
                return user_profile_error(operation, code, None);
            }
        };
        let Some(evidence) = &self.evidence else {
            return user_profile_error(operation, UserProfileErrorCode::ServiceUnavailable, None);
        };
        let audit_id = uuid::Uuid::new_v4().to_string();
        let mutation = !matches!(
            request.action,
            UserProfileAction::Read {} | UserProfileAction::Export { .. }
        );
        if mutation
            && evidence
                .begin_administration_mutation(user_profile_audit(
                    audit_id.clone(),
                    boundary,
                    &owner,
                    operation,
                    "pending",
                    None,
                ))
                .await
                .is_err()
        {
            return user_profile_error(operation, UserProfileErrorCode::Internal, None);
        }
        let result = match request.action {
            UserProfileAction::Create { profile } => store
                .create(owner.clone(), profile)
                .await
                .map(|profile| UserProfileResponse::Created {
                    version: USER_PROFILE_PROTOCOL_VERSION,
                    profile: profile.into_view(),
                }),
            UserProfileAction::Read {} => match store.read(owner.clone()).await {
                Ok(Some(profile)) => Ok(UserProfileResponse::Read {
                    version: USER_PROFILE_PROTOCOL_VERSION,
                    profile: profile.into_view(),
                }),
                Ok(None) => Ok(UserProfileResponse::NotFound {
                    version: USER_PROFILE_PROTOCOL_VERSION,
                }),
                Err(error) => Err(error),
            },
            UserProfileAction::Update {
                expected_revision,
                profile,
            } => store
                .update(owner.clone(), expected_revision, profile)
                .await
                .map(|profile| UserProfileResponse::Updated {
                    version: USER_PROFILE_PROTOCOL_VERSION,
                    profile: profile.into_view(),
                }),
            UserProfileAction::Correct {
                expected_revision,
                profile,
            } => store
                .correct(owner.clone(), expected_revision, profile)
                .await
                .map(|profile| UserProfileResponse::Corrected {
                    version: USER_PROFILE_PROTOCOL_VERSION,
                    profile: profile.into_view(),
                }),
            UserProfileAction::Export { .. } => {
                store
                    .export(owner.clone())
                    .await
                    .map(|export| UserProfileResponse::Exported {
                        version: USER_PROFILE_PROTOCOL_VERSION,
                        export,
                    })
            }
            UserProfileAction::Delete { expected_revision } => store
                .delete(owner.clone(), expected_revision)
                .await
                .map(|deleted_revision| UserProfileResponse::Deleted {
                    version: USER_PROFILE_PROTOCOL_VERSION,
                    deleted_revision,
                    do_not_learn_preserved: true,
                }),
            UserProfileAction::SetDoNotLearn {
                expected_revision,
                enabled,
            } => store
                .set_do_not_learn(owner.clone(), expected_revision, enabled)
                .await
                .map(|profile| UserProfileResponse::DoNotLearnUpdated {
                    version: USER_PROFILE_PROTOCOL_VERSION,
                    profile: profile.into_view(),
                }),
        };
        let response = result.unwrap_or_else(|error| map_user_profile_error(operation, error));
        let (outcome, error_code) = user_profile_outcome(&response);
        let audit_result = if mutation {
            evidence
                .finish_administration_mutation(audit_id, outcome, error_code)
                .await
        } else {
            evidence
                .record_administration_audit(user_profile_audit(
                    audit_id, boundary, &owner, operation, outcome, error_code,
                ))
                .await
        };
        if audit_result.is_err() {
            return user_profile_error(operation, UserProfileErrorCode::Internal, None);
        }
        response
    }

    async fn memory_confirmation(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: MemoryConfirmationRequest,
    ) -> MemoryConfirmationResponse {
        let operation = request.operation();
        let message = sylvander_protocol::UiClientMessage::MemoryConfirmation {
            request: request.clone(),
        };
        if let Err(error) = self.boundary.check(boundary, &message, operation).await {
            let code = match error.code {
                sylvander_protocol::BoundaryErrorCode::Unauthenticated => {
                    MemoryConfirmationErrorCode::Unauthenticated
                }
                sylvander_protocol::BoundaryErrorCode::PayloadTooLarge => {
                    MemoryConfirmationErrorCode::InvalidRequest
                }
                sylvander_protocol::BoundaryErrorCode::Forbidden
                | sylvander_protocol::BoundaryErrorCode::InvalidScope
                | sylvander_protocol::BoundaryErrorCode::RateLimited => {
                    MemoryConfirmationErrorCode::Forbidden
                }
            };
            return memory_confirmation_error(operation, code);
        }
        if let Err(error) = request.validate() {
            return memory_confirmation_error(
                operation,
                match error {
                    MemoryConfirmationValidationError::UnsupportedVersion => {
                        MemoryConfirmationErrorCode::UnsupportedVersion
                    }
                    MemoryConfirmationValidationError::InvalidRequest => {
                        MemoryConfirmationErrorCode::InvalidRequest
                    }
                },
            );
        }
        let session_id = SessionId::new(request.session_id());
        let Ok(session) = self.owned_session(boundary, &session_id, operation).await else {
            return memory_confirmation_error(operation, MemoryConfirmationErrorCode::Forbidden);
        };
        let Some(guardian) = &self.guardian else {
            return MemoryConfirmationResponse::service_unavailable(operation);
        };
        let now = sylvander_agent::session::now_secs();
        match request {
            MemoryConfirmationRequest::List { session_id, .. } => {
                match guardian.pending_confirmations(&session, now).await {
                    Ok(confirmations) => MemoryConfirmationResponse::Pending {
                        version: sylvander_protocol::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
                        session_id,
                        confirmations,
                    },
                    Err(error) => memory_confirmation_runtime_error(operation, &error),
                }
            }
            MemoryConfirmationRequest::Decide {
                session_id,
                candidate_id,
                expected_revision,
                decision,
                ..
            } => match guardian
                .resolve_confirmation(&session, &candidate_id, expected_revision, decision, now)
                .await
            {
                Ok(()) => MemoryConfirmationResponse::Recorded {
                    version: sylvander_protocol::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
                    session_id,
                    candidate_id,
                    decision,
                },
                Err(error) => memory_confirmation_runtime_error(operation, &error),
            },
        }
    }

    async fn identity_binding(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        identity: AuthenticatedTransportIdentity,
        request: IdentityBindingRequest,
    ) -> IdentityBindingResponse {
        let operation = request.operation();
        let Some(service) = &self.identity_bindings else {
            return identity_boundary_error(
                operation,
                IdentityBindingErrorCode::ServiceUnavailable,
                "identity binding service is unavailable",
                None,
            );
        };
        if let Err(error) = self.boundary.check_identity(boundary, &request).await {
            let code = match error.code {
                sylvander_protocol::BoundaryErrorCode::Unauthenticated => {
                    IdentityBindingErrorCode::Unauthenticated
                }
                sylvander_protocol::BoundaryErrorCode::RateLimited => {
                    IdentityBindingErrorCode::RateLimited
                }
                sylvander_protocol::BoundaryErrorCode::PayloadTooLarge => {
                    IdentityBindingErrorCode::InvalidRequest
                }
                sylvander_protocol::BoundaryErrorCode::Forbidden
                | sylvander_protocol::BoundaryErrorCode::InvalidScope => {
                    IdentityBindingErrorCode::Forbidden
                }
            };
            return identity_boundary_error(
                operation,
                code,
                "identity binding request was rejected",
                error.retry_after_ms,
            );
        }
        let (transport, channel_instance_id, principal_id) = identity.into_parts();
        service
            .dispatch(
                boundary,
                IdentityIngress::new(transport, channel_instance_id, principal_id),
                request,
            )
            .await
    }

    async fn submit_chat(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: sylvander_channel::ExternalChatRequest,
    ) -> Result<sylvander_channel::SubmittedChat, sylvander_protocol::BoundaryError> {
        let sylvander_channel::ExternalChatRequest {
            existing_session,
            mut agent_id,
            label,
            overrides,
            text,
            attachments,
            external_meta,
        } = request;
        let chat = sylvander_protocol::UiClientMessage::Chat {
            text: text.clone(),
            attachments: attachments.clone(),
            session_id: existing_session.as_ref().map(|id| id.0.clone()),
            workspace: None,
        };
        self.authorize_message(boundary, &chat).await?;
        validate_external_metadata(boundary, &external_meta)?;

        let (session_id, created_agent) = if let Some(session_id) = existing_session {
            let session = self
                .owned_session(boundary, &session_id, "submit_chat")
                .await?;
            let [session_agent] = session.agents.as_slice() else {
                return Err(sylvander_protocol::BoundaryError::forbidden(
                    boundary,
                    "submit_chat",
                ));
            };
            // A durable session owns its Agent identity. Channel defaults are
            // creation defaults and must not override a TUI-selected Agent.
            agent_id.clone_from(session_agent);
            (session_id, None)
        } else {
            let create = SessionCreateRequest {
                agent_id: agent_id.clone(),
                label,
                channel_id: Some(boundary.channel_instance_id.clone()),
                overrides,
            };
            self.authorize_message(
                boundary,
                &sylvander_protocol::UiClientMessage::CreateSession {
                    request: create.clone(),
                },
            )
            .await?;
            let created_agent = self
                .active_agent(&agent_id, boundary, "submit_chat")
                .await?;
            let state = self
                .create_session_with_metadata(boundary, create, external_meta)
                .await?;
            (state.session_id, Some(created_agent))
        };

        let submission = async {
            let events = self
                .bus
                .subscribe(SubscriptionFilter {
                    session_ids: Some(vec![session_id.clone()]),
                    recipients: None,
                    kinds: None,
                })
                .await
                .map_err(|_| {
                    boundary_failure(boundary, "submit_chat", "event relay unavailable")
                })?;
            let message = BusMessage {
                session_id: session_id.clone(),
                sender: sylvander_agent::bus::Sender::User(
                    self.effective_user_id(boundary, "submit_chat").await?.0,
                ),
                recipient: Recipient::Agent(agent_id),
                kind: sylvander_agent::bus::MessageKind::Chat,
                payload: text,
                attachments,
                timestamp: sylvander_agent::session::now_secs(),
                id: sylvander_agent::bus::MessageId::new(),
            };
            let feedback_target = self.evidence_run_id.as_ref().map(|run_id| {
                crate::evidence::feedback_target(run_id, &format!("turn:{}", message.id.0))
            });
            self.bus.publish(message).await.map_err(|_| {
                boundary_failure(boundary, "submit_chat", "message dispatch failed")
            })?;
            Ok(sylvander_channel::SubmittedChat {
                session_id: session_id.clone(),
                feedback_target,
                events,
            })
        }
        .await;
        if submission.is_err()
            && let Some(agent) = created_agent
        {
            self.rollback_created_session(&agent, &session_id, None)
                .await;
        }
        submission
    }

    async fn submit_control(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        message: sylvander_protocol::UiClientMessage,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        use sylvander_protocol::UiClientMessage as ClientMessage;

        let session_id = ui_session_id(&message)
            .map(SessionId::new)
            .ok_or_else(|| boundary_failure(boundary, "submit_control", "session is required"))?;
        self.authorize_message(boundary, &message).await?;
        let session = self
            .owned_session(boundary, &session_id, "submit_control")
            .await?;
        let agent_id = session.agents.first().cloned().ok_or_else(|| {
            sylvander_protocol::BoundaryError::forbidden(boundary, "submit_control")
        })?;
        let system = match message {
            ClientMessage::Approve {
                call_id,
                approved,
                scope,
                reason,
                ..
            } => sylvander_agent::bus::SystemMessage::ApproveTool {
                call_id,
                approved,
                scope,
                reason,
            },
            ClientMessage::Answer {
                call_id, answer, ..
            } => sylvander_agent::bus::SystemMessage::AnswerQuestion { call_id, answer },
            ClientMessage::Interrupt { .. } => sylvander_agent::bus::SystemMessage::InterruptTurn {
                session_id: session_id.clone(),
            },
            ClientMessage::ResolvePlan {
                plan_id, decision, ..
            } => sylvander_agent::bus::SystemMessage::ResolvePlan { plan_id, decision },
            ClientMessage::CancelTask { task_id, .. } => {
                sylvander_agent::bus::SystemMessage::CancelTask {
                    session_id: session_id.clone(),
                    task_id,
                }
            }
            _ => {
                return Err(boundary_failure(
                    boundary,
                    "submit_control",
                    "unsupported interactive control",
                ));
            }
        };
        self.bus
            .publish(BusMessage {
                session_id,
                sender: sylvander_agent::bus::Sender::System,
                recipient: Recipient::Agent(agent_id),
                kind: sylvander_agent::bus::MessageKind::System(system),
                payload: String::new(),
                attachments: Vec::new(),
                timestamp: sylvander_agent::session::now_secs(),
                id: sylvander_agent::bus::MessageId::new(),
            })
            .await
            .map_err(|_| boundary_failure(boundary, "submit_control", "control dispatch failed"))
    }

    async fn context_report(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<sylvander_protocol::ContextReport, sylvander_protocol::BoundaryError> {
        let (session, agent) = self
            .owned_session_agent(boundary, session_id, "context_report")
            .await?;
        Ok(agent.run.context_report(Some(&session.id)).await)
    }

    async fn compact_session(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<sylvander_protocol::CompactionReport, sylvander_protocol::BoundaryError> {
        let (_, agent) = self
            .owned_session_agent(boundary, session_id, "compact_session")
            .await?;
        agent
            .run
            .compact_session(session_id)
            .await
            .map_err(|error| boundary_failure(boundary, "compact_session", error))
    }

    async fn preview_workspace_rollback(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<sylvander_protocol::WorkspaceRollbackPreview, sylvander_protocol::BoundaryError>
    {
        let (_, agent) = self
            .owned_session_agent(boundary, session_id, "preview_workspace_rollback")
            .await?;
        let preview = agent
            .run
            .preview_workspace_rollback(session_id)
            .await
            .map_err(|error| boundary_failure(boundary, "preview_workspace_rollback", error))?;
        Ok(sylvander_protocol::WorkspaceRollbackPreview {
            turn_id: preview.turn_id,
            files: preview.files,
        })
    }

    async fn rollback_workspace(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
        expected_turn_id: &str,
    ) -> Result<sylvander_protocol::WorkspaceRollbackReport, sylvander_protocol::BoundaryError>
    {
        let (_, agent) = self
            .owned_session_agent(boundary, session_id, "rollback_workspace")
            .await?;
        let report = agent
            .run
            .rollback_workspace_latest(session_id, expected_turn_id)
            .await
            .map_err(|error| boundary_failure(boundary, "rollback_workspace", error))?;
        Ok(sylvander_protocol::WorkspaceRollbackReport {
            turn_id: report.turn_id,
            restored: report.restored,
        })
    }

    async fn inspect_coding_session(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<sylvander_protocol::CodingSessionDiff, sylvander_protocol::BoundaryError> {
        let session = self
            .owned_session(boundary, session_id, "inspect_coding_session")
            .await?;
        let manager = self.worktrees.clone().ok_or_else(|| {
            boundary_failure(
                boundary,
                "inspect_coding_session",
                "coding worktrees are unavailable",
            )
        })?;
        let target = coding_worktree_target(&session)
            .map_err(|error| boundary_failure(boundary, "inspect_coding_session", error))?;
        let diff = manager
            .inspect(&session_id.0, target)
            .await
            .map_err(|error| boundary_failure(boundary, "inspect_coding_session", error))?;
        Ok(sylvander_protocol::CodingSessionDiff {
            status: diff.status,
            patch: diff.patch,
        })
    }

    async fn accept_coding_session(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        let session = self
            .owned_session(boundary, session_id, "accept_coding_session")
            .await?;
        let manager = self.worktrees.clone().ok_or_else(|| {
            boundary_failure(
                boundary,
                "accept_coding_session",
                "coding worktrees are unavailable",
            )
        })?;
        let target = coding_worktree_target(&session)
            .map_err(|error| boundary_failure(boundary, "accept_coding_session", error))?;
        manager
            .accept(&session_id.0, target)
            .await
            .map_err(|error| boundary_failure(boundary, "accept_coding_session", error))
    }

    async fn discard_coding_session(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        let (session, agent) = self
            .owned_session_agent(boundary, session_id, "discard_coding_session")
            .await?;
        let manager = self.worktrees.clone().ok_or_else(|| {
            boundary_failure(
                boundary,
                "discard_coding_session",
                "coding worktrees are unavailable",
            )
        })?;
        let target = coding_worktree_target(&session)
            .map_err(|error| boundary_failure(boundary, "discard_coding_session", error))?;
        manager
            .discard(&session_id.0, target)
            .await
            .map_err(|error| boundary_failure(boundary, "discard_coding_session", error))?;
        self.engine.detach_session(session_id).await;
        agent.detach_authenticated_session(session_id).await;
        self.sessions.delete(&session.id).await.map_err(|error| {
            boundary_failure(boundary, "discard_coding_session", error.to_string())
        })?;
        if let Some(guardian) = &self.guardian
            && let Err(error) = guardian
                .enqueue_session_closed(&session, sylvander_agent::session::now_secs())
                .await
        {
            warn!(
                %error,
                session_id = %session.id,
                "failed to enqueue closed session for Guardian curation"
            );
        }
        Ok(())
    }

    async fn delete_session(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        let (session, agent) = self
            .owned_session_agent(boundary, session_id, "delete_session")
            .await?;
        self.engine.detach_session(session_id).await;
        agent.detach_authenticated_session(session_id).await;
        self.sessions
            .delete(session_id)
            .await
            .map_err(|error| boundary_failure(boundary, "delete_session", error.to_string()))?;
        let target = coding_worktree_target(&session)
            .map_err(|error| boundary_failure(boundary, "delete_session", error))?;
        self.discard_worktree(session_id, target).await;
        if let Some(guardian) = &self.guardian
            && let Err(error) = guardian
                .enqueue_session_closed(&session, sylvander_agent::session::now_secs())
                .await
        {
            warn!(
                %error,
                %session_id,
                "failed to enqueue deleted session for Guardian curation"
            );
        }
        Ok(())
    }

    async fn agent_admin(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: AgentAdminRequest,
    ) -> AgentAdminResponse {
        let (Some(registry), Some(provider)) = (
            self.agent_registry.as_ref(),
            self.revision_provider.as_ref(),
        ) else {
            return agent_admin_error(
                AgentAdminErrorCode::StorageUnavailable,
                "Agent administration service is unavailable",
            );
        };
        let audit_id = match agent_admin_mutation(&request) {
            Some((operation, agent_id, revision, expected_active_revision))
                if is_agent_administrator(boundary.principal.as_ref()) =>
            {
                let Some(store) = &self.evidence else {
                    return agent_admin_error(
                        AgentAdminErrorCode::StorageUnavailable,
                        "Agent administration audit is unavailable",
                    );
                };
                let principal = boundary.principal.as_ref().expect("administrator checked");
                let id = uuid::Uuid::new_v4().to_string();
                if store
                    .begin_agent_administration(crate::evidence::AgentAdministrationAudit {
                        id: id.clone(),
                        occurred_at: sylvander_agent::session::now_secs(),
                        request_id: boundary.request_id.clone(),
                        principal_digest: sha256_text(&principal.id.0),
                        channel_instance_id: boundary.channel_instance_id.clone(),
                        operation: operation.into(),
                        agent_digest: sha256_text(&agent_id.0),
                        revision,
                        expected_active_revision,
                        outcome: "pending".into(),
                        error_code: None,
                    })
                    .await
                    .is_err()
                {
                    return agent_admin_error(
                        AgentAdminErrorCode::StorageUnavailable,
                        "Agent administration audit is unavailable",
                    );
                }
                Some(id)
            }
            _ => None,
        };
        let response = match AgentAdminService::new(registry, &provider.config)
            .dispatch(boundary.principal.as_ref(), request)
            .await
        {
            AgentAdminDispatch::Response(response) => response,
            AgentAdminDispatch::Update {
                expected_active_revision,
                definition,
            } => {
                let selection = active_snapshot_selection(&definition);
                match registry
                    .stage_agent_revision_v3(expected_active_revision, *definition, selection)
                    .await
                {
                    Ok((stored, _)) => {
                        if provider
                            .revalidate_revision(
                                &stored.definition.spec.id,
                                stored.definition.revision,
                            )
                            .await
                            .is_err()
                        {
                            agent_admin_error(
                                AgentAdminErrorCode::InvalidDefinition,
                                "Agent revision could not be composed",
                            )
                        } else {
                            AgentAdminResponse::Success {
                                result: Box::new(AgentAdminResult::DefinitionUpdated {
                                    revision: redact_revision(&stored),
                                }),
                            }
                        }
                    }
                    Err(AgentSnapshotV3Error::Registry(error)) => AgentAdminResponse::Error {
                        error: map_registry_error(error),
                    },
                    Err(_) => agent_admin_error(
                        AgentAdminErrorCode::InvalidDefinition,
                        "Agent revision could not be composed",
                    ),
                }
            }
            AgentAdminDispatch::Activate {
                agent_id,
                revision,
                expected_active_revision,
            } => match provider.revalidate_revision(&agent_id, revision).await {
                Err(_) => agent_admin_error(
                    AgentAdminErrorCode::InvalidDefinition,
                    "Agent revision could not be composed",
                ),
                Ok(_) => match registry
                    .activate(&agent_id, revision, expected_active_revision)
                    .await
                {
                    Ok(()) => AgentAdminResponse::Success {
                        result: Box::new(AgentAdminResult::RevisionActivated {
                            agent_id,
                            active_revision: revision,
                        }),
                    },
                    Err(error) => AgentAdminResponse::Error {
                        error: map_registry_error(error),
                    },
                },
            },
            AgentAdminDispatch::Rollback {
                agent_id,
                target_revision,
                expected_active_revision,
            } => match provider
                .revalidate_revision(&agent_id, target_revision)
                .await
            {
                Err(_) => agent_admin_error(
                    AgentAdminErrorCode::InvalidDefinition,
                    "Agent revision could not be composed",
                ),
                Ok(_) => match registry
                    .rollback(&agent_id, target_revision, expected_active_revision)
                    .await
                {
                    Ok(()) => AgentAdminResponse::Success {
                        result: Box::new(AgentAdminResult::RevisionRolledBack {
                            agent_id,
                            active_revision: target_revision,
                        }),
                    },
                    Err(error) => AgentAdminResponse::Error {
                        error: map_registry_error(error),
                    },
                },
            },
        };
        if let (Some(id), Some(store)) = (audit_id, self.evidence.as_ref()) {
            let (outcome, error_code) = match &response {
                AgentAdminResponse::Success { .. } => ("succeeded", None),
                AgentAdminResponse::Error { error } => {
                    ("failed", Some(agent_admin_error_code(error.code)))
                }
            };
            if let Err(error) = store
                .finish_agent_administration(id, outcome, error_code)
                .await
            {
                warn!(%error, "failed to finish Agent administration audit");
            }
        }
        response
    }

    async fn registry_admin(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: RegistryAdminRequest,
    ) -> RegistryAdminResponse {
        let Some(registry) = self.agent_registry.as_ref() else {
            return registry_admin_error(
                RegistryAdminErrorCode::StorageUnavailable,
                "Registry administration service is unavailable",
            );
        };
        let Some(principal) = boundary.principal.as_ref() else {
            return RegistryAdminService::new(registry)
                .dispatch(None, request)
                .await;
        };
        if !is_agent_administrator(Some(principal)) {
            return RegistryAdminService::new(registry)
                .dispatch(Some(principal), request)
                .await;
        }
        let Some(store) = self.evidence.as_ref() else {
            return registry_admin_error(
                RegistryAdminErrorCode::StorageUnavailable,
                "Registry administration audit is unavailable",
            );
        };
        let target = registry_admin_audit_target(&request);
        if registry_admin_is_mutation(&request) {
            let credential_resolver = if registry_admin_is_credential_mutation(&request) {
                let Some(resolver) = self.credential_resolver.as_ref() else {
                    return registry_admin_error(
                        RegistryAdminErrorCode::StorageUnavailable,
                        "Credential mutation service is unavailable",
                    );
                };
                Some(resolver.as_ref())
            } else {
                None
            };
            let audit_id = uuid::Uuid::new_v4().to_string();
            let intent = registry_administration_audit(
                audit_id.clone(),
                boundary,
                principal,
                &target,
                "pending",
                None,
            );
            if store.begin_administration_mutation(intent).await.is_err() {
                return registry_admin_error(
                    RegistryAdminErrorCode::StorageUnavailable,
                    "Registry administration audit is unavailable",
                );
            }
            let response = if let Some(resolver) = credential_resolver {
                let Some(audit) = self.credential_audit.as_deref() else {
                    return registry_admin_error(
                        RegistryAdminErrorCode::StorageUnavailable,
                        "Credential operation audit is unavailable",
                    );
                };
                CredentialRegistryMutationService::new(registry, resolver, audit)
                    .dispatch(Some(principal), request)
                    .await
            } else {
                RegistryAdminService::new(registry)
                    .dispatch(Some(principal), request)
                    .await
            };
            let (outcome, error_code) = registry_admin_outcome(&response);
            if let Err(error) = store
                .finish_administration_mutation(audit_id, outcome, error_code)
                .await
            {
                warn!(%error, "failed to finish registry administration audit");
                return registry_admin_error(
                    RegistryAdminErrorCode::StorageUnavailable,
                    "Registry administration audit is unavailable",
                );
            }
            return response;
        }
        let response = RegistryAdminService::new(registry)
            .dispatch(Some(principal), request)
            .await;
        let (outcome, error_code) = registry_admin_outcome(&response);
        let audit = registry_administration_audit(
            uuid::Uuid::new_v4().to_string(),
            boundary,
            principal,
            &target,
            outcome,
            error_code,
        );
        if store.record_administration_audit(audit).await.is_err() {
            return registry_admin_error(
                RegistryAdminErrorCode::StorageUnavailable,
                "Registry administration audit is unavailable",
            );
        }
        response
    }
}

impl RuntimeUiService {
    async fn create_session_with_metadata(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: SessionCreateRequest,
        external_meta: BTreeMap<String, String>,
    ) -> Result<SessionConfigState, sylvander_protocol::BoundaryError> {
        let user_id = self.effective_user_id(boundary, "create_session").await?;
        let label = request.label.trim().to_string();
        if label.is_empty() || label.len() > 200 {
            return Err(boundary_failure(
                boundary,
                "create_session",
                "session label must contain 1..=200 bytes",
            ));
        }
        if request
            .channel_id
            .as_deref()
            .is_some_and(|id| id != boundary.channel_instance_id)
        {
            return Err(sylvander_protocol::BoundaryError::forbidden(
                boundary,
                "create_session",
            ));
        }
        let agent = self
            .active_agent(&request.agent_id, boundary, "create_session")
            .await?;
        if !self
            .current_agent_access_allowed(&request.agent_id, boundary, "create_session")
            .await?
        {
            return Err(sylvander_protocol::BoundaryError::forbidden(
                boundary,
                "create_session",
            ));
        }
        let mut effective = resolve_session_config(&agent, &request.overrides, None, None)
            .map_err(|error| boundary_failure(boundary, "create_session", error.to_string()))?;
        let workspace_binding = effective
            .user_workspace
            .as_ref()
            .or(effective.agent_workspace.as_ref());
        if let Some(worktrees) = self.worktrees.as_ref() {
            ensure_remote_mutation_mounts_are_transactional(&effective, worktrees)
                .map_err(|error| boundary_failure(boundary, "create_session", error))?;
        }
        let worktree_target = workspace_binding
            .filter(|binding| !binding.read_only)
            .map(|binding| binding.execution_target.clone());
        let mut workspace = workspace_binding.map_or_else(
            || std::path::PathBuf::from("."),
            |binding| binding.path.clone(),
        );
        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let lease = match (worktree_target.as_deref(), self.worktrees.as_ref()) {
            (Some(target), Some(manager)) => manager
                .create(&session_id.0, target, &workspace)
                .await
                .map_err(|error| boundary_failure(boundary, "create_session", error))?,
            _ => None,
        };
        if let Some(lease) = &lease {
            workspace.clone_from(&lease.effective_workspace);
        }
        bind_effective_workspace(&mut effective, &workspace);
        let metadata = SessionMetadata {
            workspace,
            name: label.clone(),
            user_id: user_id.0,
        };
        let mut session = StoredSession::new(
            session_id.clone(),
            &label,
            SessionLifetime::Persistent,
            metadata.clone(),
            vec![request.agent_id.clone()],
        );
        session.config_overrides = request.overrides.clone();
        session.effective_config = Some(effective.clone());
        session.external_meta.extend(
            external_meta
                .into_iter()
                .map(|(key, value)| (key, serde_json::Value::String(value))),
        );
        session.external_meta.insert(
            "channel_id".into(),
            serde_json::Value::String(boundary.channel_instance_id.clone()),
        );
        if let Some(lease) = &lease {
            session.external_meta.insert(
                "git_worktree".into(),
                serde_json::Value::String(lease.branch.clone()),
            );
            if let Some(target) = &lease.target_id {
                session.external_meta.insert(
                    "git_worktree_target".into(),
                    serde_json::Value::String(target.clone()),
                );
            }
        }
        if let Err(error) = self.sessions.save(&session).await {
            self.discard_worktree(
                &session_id,
                lease.as_ref().and_then(|lease| lease.target_id.as_deref()),
            )
            .await;
            return Err(boundary_failure(
                boundary,
                "create_session",
                error.to_string(),
            ));
        }
        if let Err(error) = agent
            .attach_authenticated_session(session_id.clone(), metadata.clone())
            .await
        {
            let _ = self.sessions.delete(&session_id).await;
            self.discard_worktree(
                &session_id,
                lease.as_ref().and_then(|lease| lease.target_id.as_deref()),
            )
            .await;
            return Err(boundary_failure(
                boundary,
                "create_session",
                error.to_string(),
            ));
        }
        if let Err(error) = self
            .engine
            .attach_session(
                session_id.clone(),
                &label,
                metadata,
                std::slice::from_ref(&request.agent_id),
            )
            .await
        {
            self.rollback_created_session(
                &agent,
                &session_id,
                lease.as_ref().and_then(|lease| lease.target_id.clone()),
            )
            .await;
            return Err(boundary_failure(
                boundary,
                "create_session",
                error.to_string(),
            ));
        }
        if let Some(guardian) = &self.guardian
            && let Err(error) = guardian
                .audit_worker_session_binding(&session, sylvander_agent::session::now_secs())
                .await
        {
            self.rollback_created_session(
                &agent,
                &session_id,
                lease.as_ref().and_then(|lease| lease.target_id.clone()),
            )
            .await;
            return Err(boundary_failure(
                boundary,
                "create_session",
                error.to_string(),
            ));
        }
        Ok(SessionConfigState {
            session_id,
            revision: 0,
            overrides: request.overrides,
            effective,
        })
    }

    async fn rollback_created_session(
        &self,
        agent: &ConfiguredAgent,
        session_id: &SessionId,
        target_id: Option<String>,
    ) {
        self.engine.detach_session(session_id).await;
        agent.detach_authenticated_session(session_id).await;
        let target_id = match target_id {
            Some(target) => Some(target),
            None => match self.sessions.get(session_id).await {
                Ok(Some(session)) => coding_worktree_target(&session)
                    .ok()
                    .flatten()
                    .map(str::to_owned),
                _ => None,
            },
        };
        if let Err(error) = self.sessions.delete(session_id).await {
            warn!(%error, %session_id, "failed to delete compensated session");
        }
        self.discard_worktree(session_id, target_id.as_deref())
            .await;
    }

    async fn discard_worktree(&self, session_id: &SessionId, target_id: Option<&str>) {
        let Some(manager) = self.worktrees.clone() else {
            return;
        };
        if let Err(error) = manager.discard_if_present(&session_id.0, target_id).await {
            warn!(%error, %session_id, "failed to discard session worktree");
        }
    }

    async fn active_agent(
        &self,
        agent_id: &AgentId,
        boundary: &sylvander_protocol::BoundaryContext,
        operation: &str,
    ) -> Result<ConfiguredAgent, sylvander_protocol::BoundaryError> {
        if let Some(provider) = &self.revision_provider {
            return provider
                .active_agent(agent_id)
                .await
                .map_err(|error| boundary_failure(boundary, operation, error.to_string()));
        }
        self.agents.get(agent_id).cloned().ok_or_else(|| {
            boundary_failure(boundary, operation, format!("unknown Agent {agent_id}"))
        })
    }

    async fn current_agent_access_allowed(
        &self,
        agent_id: &AgentId,
        boundary: &sylvander_protocol::BoundaryContext,
        operation: &str,
    ) -> Result<bool, sylvander_protocol::BoundaryError> {
        if privileged_principal(boundary) {
            return Ok(true);
        }
        if let Some(registry) = &self.agent_registry {
            let active = registry
                .load_active(agent_id)
                .await
                .map_err(|error| boundary_failure(boundary, operation, error.to_string()))?;
            return Ok(active.is_some_and(|revision| {
                agent_access_allowed(&revision.definition.access, boundary)
            }));
        }
        Ok(self
            .agents
            .get(agent_id)
            .is_some_and(|agent| agent_access_allowed(&agent.definition.access, boundary)))
    }

    async fn bind_session_revision(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session: &StoredSession,
        mut agent: ConfiguredAgent,
        operation: &'static str,
    ) -> Result<ConfiguredAgent, sylvander_protocol::BoundaryError> {
        let Some(effective) = &session.effective_config else {
            return Ok(agent);
        };
        if let Some(provider) = &self.revision_provider {
            return provider
                .configured_revision(&effective.agent_id, effective.agent_revision)
                .await
                .map_err(|error| boundary_failure(boundary, operation, error.to_string()));
        }
        if effective.agent_revision == agent.definition.revision {
            return Ok(agent);
        }
        let registry = self.agent_registry.as_ref().ok_or_else(|| {
            boundary_failure(boundary, operation, "Agent registry is unavailable")
        })?;
        let revision = registry
            .load(&effective.agent_id, effective.agent_revision)
            .await
            .map_err(|error| boundary_failure(boundary, operation, error.to_string()))?
            .ok_or_else(|| {
                boundary_failure(
                    boundary,
                    operation,
                    format!(
                        "unknown Agent revision {}@{}",
                        effective.agent_id, effective.agent_revision
                    ),
                )
            })?;
        agent.definition = revision.definition;
        Ok(agent)
    }

    async fn authorize_message_inner(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        message: &sylvander_protocol::UiClientMessage,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        require_principal(boundary, ui_operation(message))?;
        if matches!(
            message,
            sylvander_protocol::UiClientMessage::AgentAdmin { .. }
                | sylvander_protocol::UiClientMessage::RegistryAdmin { .. }
        ) && !is_agent_administrator(boundary.principal.as_ref())
        {
            return Err(sylvander_protocol::BoundaryError::forbidden(
                boundary,
                ui_operation(message),
            ));
        }
        if let sylvander_protocol::UiClientMessage::CreateSession { request } = message {
            self.agents.get(&request.agent_id).ok_or_else(|| {
                sylvander_protocol::BoundaryError::forbidden(boundary, "create_session")
            })?;
            if !self
                .current_agent_access_allowed(&request.agent_id, boundary, "create_session")
                .await?
            {
                return Err(sylvander_protocol::BoundaryError::forbidden(
                    boundary,
                    "create_session",
                ));
            }
        }
        if let sylvander_protocol::UiClientMessage::SubmitFeedback { feedback } = message {
            let store = self.evidence.as_ref().ok_or_else(|| {
                boundary_failure(
                    boundary,
                    "submit_feedback",
                    "security audit store is unavailable",
                )
            })?;
            let session_id = store
                .feedback_session(feedback.target.clone())
                .await
                .map_err(|error| boundary_failure(boundary, "submit_feedback", error.to_string()))?
                .ok_or_else(|| {
                    boundary_failure(
                        boundary,
                        "submit_feedback",
                        "feedback must identify one attributable session",
                    )
                })?;
            self.owned_session(boundary, &SessionId::new(session_id), "submit_feedback")
                .await?;
        }
        if matches!(
            message,
            sylvander_protocol::UiClientMessage::SelectModel {
                session_id: None,
                ..
            } | sylvander_protocol::UiClientMessage::SelectPermissions {
                session_id: None,
                ..
            }
        ) {
            return Err(sylvander_protocol::BoundaryError::forbidden(
                boundary,
                ui_operation(message),
            ));
        }
        if let Some(session_id) = ui_session_id(message) {
            self.owned_session(boundary, &SessionId::new(session_id), ui_operation(message))
                .await?;
        }
        Ok(())
    }

    async fn record_denial(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        message: &sylvander_protocol::UiClientMessage,
        error: &sylvander_protocol::BoundaryError,
    ) -> Result<(), String> {
        self.record_boundary_denial(
            boundary,
            ui_operation(message),
            ui_session_id(message),
            error,
        )
        .await
    }

    async fn record_boundary_denial(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        operation: &str,
        resource_id: Option<&str>,
        error: &sylvander_protocol::BoundaryError,
    ) -> Result<(), String> {
        let store = self
            .evidence
            .as_ref()
            .ok_or_else(|| "security audit store is unavailable".to_string())?;
        let code = match error.code {
            sylvander_protocol::BoundaryErrorCode::Unauthenticated => "unauthenticated",
            sylvander_protocol::BoundaryErrorCode::Forbidden => "forbidden",
            sylvander_protocol::BoundaryErrorCode::InvalidScope => "invalid_scope",
            sylvander_protocol::BoundaryErrorCode::PayloadTooLarge => "payload_too_large",
            sylvander_protocol::BoundaryErrorCode::RateLimited => "rate_limited",
        };
        store
            .record_authorization_denial(AuthorizationDenial {
                id: uuid::Uuid::new_v4().to_string(),
                occurred_at: sylvander_agent::session::now_secs(),
                request_id: boundary.request_id.clone(),
                principal_digest: boundary
                    .principal
                    .as_ref()
                    .map(|principal| sha256_text(&principal.id.0)),
                channel_instance_id: boundary.channel_instance_id.clone(),
                transport: boundary.transport.clone(),
                operation: operation.into(),
                code: code.into(),
                resource_digest: resource_id.map(sha256_text),
            })
            .await
            .map_err(|audit_error| audit_error.to_string())
    }

    async fn owned_session(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
        operation: &str,
    ) -> Result<StoredSession, sylvander_protocol::BoundaryError> {
        let user_id = self.effective_user_id(boundary, operation).await?;
        let session = self
            .sessions
            .get(session_id)
            .await
            .map_err(|error| boundary_failure(boundary, operation, error.to_string()))?
            .ok_or_else(|| sylvander_protocol::BoundaryError::forbidden(boundary, operation))?;
        if session.metadata.user_id != user_id.0 && !privileged_principal(boundary) {
            return Err(sylvander_protocol::BoundaryError::forbidden(
                boundary, operation,
            ));
        }
        if session.agents.is_empty() {
            return Err(sylvander_protocol::BoundaryError::forbidden(
                boundary, operation,
            ));
        }
        for agent_id in &session.agents {
            if !self
                .current_agent_access_allowed(agent_id, boundary, operation)
                .await?
            {
                return Err(sylvander_protocol::BoundaryError::forbidden(
                    boundary, operation,
                ));
            }
        }
        if let Some(provider) = &self.revision_provider {
            let agent_id = session.agents.first().expect("non-empty checked above");
            provider
                .bound_stored_revision(agent_id, &session)
                .await
                .map_err(|_| {
                    boundary_failure(boundary, operation, "session registry binding is invalid")
                })?;
        }
        Ok(session)
    }

    async fn effective_user_id(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        operation: &str,
    ) -> Result<UserId, sylvander_protocol::BoundaryError> {
        let principal = require_principal(boundary, operation)?;
        if principal.kind == sylvander_protocol::PrincipalKind::User
            && let Some(service) = &self.identity_bindings
        {
            return service
                .resolve_user(
                    boundary,
                    IdentityIngress::new(
                        boundary.transport.clone(),
                        boundary.channel_instance_id.clone(),
                        principal.id.0.clone(),
                    ),
                )
                .await
                .map_err(|_| {
                    boundary_failure(boundary, operation, "stable user identity is unavailable")
                });
        }
        let scoped = format!(
            "sylvander.unlinked-principal.v1\0{}\0{}\0{}",
            boundary.transport, boundary.channel_instance_id, principal.id.0
        );
        Ok(UserId::new(format!("unlinked:v1:{}", sha256_text(&scoped))))
    }

    async fn owned_session_agent(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
        operation: &str,
    ) -> Result<(StoredSession, ConfiguredAgent), sylvander_protocol::BoundaryError> {
        let session = self.owned_session(boundary, session_id, operation).await?;
        let agent_id = session
            .agents
            .first()
            .expect("owned_session rejects empty agent bindings");
        let agent = if let (Some(provider), Some(effective)) =
            (&self.revision_provider, &session.effective_config)
        {
            if &effective.agent_id != agent_id {
                return Err(sylvander_protocol::BoundaryError::forbidden(
                    boundary, operation,
                ));
            }
            provider
                .configured_revision(agent_id, effective.agent_revision)
                .await
                .map_err(|_| {
                    boundary_failure(boundary, operation, "session Agent is unavailable")
                })?
        } else {
            self.agents.get(agent_id).cloned().ok_or_else(|| {
                boundary_failure(boundary, operation, "session Agent is unavailable")
            })?
        };
        Ok((session, agent))
    }
}

#[cfg(test)]
fn execution_target_supports_host_worktree(
    targets: &HashMap<String, config::ExecutionTransportConfig>,
    target_id: &str,
) -> bool {
    matches!(
        targets.get(target_id),
        Some(
            config::ExecutionTransportConfig::Local { .. }
                | config::ExecutionTransportConfig::Container { .. }
                | config::ExecutionTransportConfig::Sandbox { .. }
        )
    )
}

fn coding_worktree_target(session: &StoredSession) -> Result<Option<&str>, String> {
    match session.external_meta.get("git_worktree_target") {
        Some(serde_json::Value::String(target)) if !target.trim().is_empty() => {
            Ok(Some(target.as_str()))
        }
        Some(_) => Err("session has invalid remote worktree metadata".into()),
        None => Ok(None),
    }
}

fn build_coding_worktree_service(
    config: &ServerConfig,
) -> Result<coding_worktree::CodingWorktreeService, RuntimeError> {
    let data_dir = config
        .server
        .data_dir
        .as_ref()
        .expect("runtime data directory is resolved before worktree composition");
    let local = Arc::new(git_worktree::GitWorktreeManager::new(
        data_dir.join("coding-sessions"),
    ));
    let mut service = coding_worktree::CodingWorktreeService::new(local);
    let mut explicit_local = false;
    for target in &config.execution_targets {
        match &target.transport {
            config::ExecutionTransportConfig::Ssh {
                host,
                port,
                user,
                credential,
                known_hosts,
                control_path,
                worktree_root,
            } => {
                let identity = SystemSecretResolver.resolve(credential).map_err(|_| {
                    RuntimeError::Config(format!(
                        "SSH target {} identity resolution failed",
                        target.id
                    ))
                })?;
                let identity_path = identity.as_str().map_err(|_| {
                    RuntimeError::Config(format!(
                        "SSH target {} identity must be a UTF-8 path",
                        target.id
                    ))
                })?;
                let executor = crate::execution::SshExecutor::new(
                    host,
                    *port,
                    user,
                    identity_path,
                    known_hosts,
                    control_path,
                )
                .map_err(|_| {
                    RuntimeError::Config(format!(
                        "SSH target {} executor configuration is invalid",
                        target.id
                    ))
                })?;
                let manager = remote_git_worktree::RemoteGitWorktreeManager::new(
                    data_dir
                        .join("coding-sessions/remote")
                        .join(sha256_text(&target.id)),
                    worktree_root,
                    target.id.clone(),
                    Arc::new(executor),
                )
                .map_err(|error| {
                    RuntimeError::Config(format!("SSH target {}: {error}", target.id))
                })?;
                service
                    .register_remote(target.id.clone(), Arc::new(manager))
                    .map_err(RuntimeError::Config)?;
            }
            config::ExecutionTransportConfig::Local { .. }
            | config::ExecutionTransportConfig::Container { .. }
            | config::ExecutionTransportConfig::Sandbox { .. } => {
                explicit_local |= target.id == "local";
                service
                    .register_local(target.id.clone())
                    .map_err(RuntimeError::Config)?;
            }
        }
    }
    if !explicit_local {
        service
            .register_local("local")
            .map_err(RuntimeError::Config)?;
    }
    Ok(service)
}

fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn privileged_principal(boundary: &sylvander_protocol::BoundaryContext) -> bool {
    boundary.principal.as_ref().is_some_and(|principal| {
        principal.kind == sylvander_protocol::PrincipalKind::System || principal.has_role("admin")
    })
}

fn agent_access_allowed(
    access: &crate::config::AgentAccessConfig,
    boundary: &sylvander_protocol::BoundaryContext,
) -> bool {
    let Some(principal) = &boundary.principal else {
        return false;
    };
    privileged_principal(boundary)
        || access.allow_authenticated
        || access
            .allowed_principals
            .iter()
            .any(|allowed| allowed == &principal.id.0)
        || principal
            .roles
            .iter()
            .any(|role| access.allowed_roles.contains(role))
}

fn require_principal<'a>(
    boundary: &'a sylvander_protocol::BoundaryContext,
    operation: &str,
) -> Result<&'a sylvander_protocol::AuthenticatedPrincipal, sylvander_protocol::BoundaryError> {
    boundary
        .principal
        .as_ref()
        .ok_or_else(|| sylvander_protocol::BoundaryError::unauthenticated(boundary, operation))
}

fn valid_evidence_references(references: &[sylvander_protocol::EvidenceReference]) -> bool {
    references.len() <= 16
        && references.iter().all(|reference| {
            !reference.locator.trim().is_empty()
                && reference.locator.len() <= 1024
                && reference.digest_sha256.as_ref().is_none_or(|digest| {
                    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        })
}

fn boundary_failure(
    boundary: &sylvander_protocol::BoundaryContext,
    operation: &str,
    message: impl Into<String>,
) -> sylvander_protocol::BoundaryError {
    sylvander_protocol::BoundaryError {
        code: sylvander_protocol::BoundaryErrorCode::InvalidScope,
        operation: operation.into(),
        request_id: boundary.request_id.clone(),
        message: message.into(),
        retry_after_ms: None,
    }
}

fn memory_confirmation_runtime_error(
    operation: &str,
    error: &GuardianRuntimeError,
) -> MemoryConfirmationResponse {
    let code = match error {
        GuardianRuntimeError::Curation(guardian_curation::GuardianCurationError::AccessDenied) => {
            MemoryConfirmationErrorCode::Forbidden
        }
        GuardianRuntimeError::Curation(
            guardian_curation::GuardianCurationError::Conflict
            | guardian_curation::GuardianCurationError::IdempotencyConflict,
        ) => MemoryConfirmationErrorCode::Conflict,
        GuardianRuntimeError::Curation(guardian_curation::GuardianCurationError::InvalidInput)
        | GuardianRuntimeError::InvalidConfiguration => MemoryConfirmationErrorCode::InvalidRequest,
        _ => MemoryConfirmationErrorCode::ServiceUnavailable,
    };
    memory_confirmation_error(operation, code)
}

fn memory_confirmation_error(
    operation: &str,
    code: MemoryConfirmationErrorCode,
) -> MemoryConfirmationResponse {
    let message = match code {
        MemoryConfirmationErrorCode::UnsupportedVersion => {
            "memory confirmation protocol version is unsupported"
        }
        MemoryConfirmationErrorCode::InvalidRequest => "memory confirmation request is invalid",
        MemoryConfirmationErrorCode::Unauthenticated => {
            "memory confirmation requires authentication"
        }
        MemoryConfirmationErrorCode::Forbidden => {
            "memory confirmation is unavailable for this session"
        }
        MemoryConfirmationErrorCode::Conflict => "memory confirmation is stale or already resolved",
        MemoryConfirmationErrorCode::ServiceUnavailable => {
            "memory confirmation service is unavailable"
        }
    };
    MemoryConfirmationResponse::Error {
        version: sylvander_protocol::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
        operation: operation.into(),
        code,
        message: message.into(),
    }
}

fn map_user_profile_error(
    operation: UserProfileOperation,
    error: UserProfileStoreError,
) -> UserProfileResponse {
    let (code, current_revision) = match error {
        UserProfileStoreError::Invalid(_) => (UserProfileErrorCode::InvalidRequest, None),
        UserProfileStoreError::AlreadyExists => (UserProfileErrorCode::AlreadyExists, None),
        UserProfileStoreError::NotFound => (UserProfileErrorCode::NotFound, None),
        UserProfileStoreError::Conflict { actual, .. } => {
            (UserProfileErrorCode::Conflict, Some(actual))
        }
        UserProfileStoreError::IncompatibleSchema
        | UserProfileStoreError::Corrupt
        | UserProfileStoreError::Storage
        | UserProfileStoreError::Task => (UserProfileErrorCode::Internal, None),
    };
    user_profile_error(operation, code, current_revision)
}

fn user_profile_error(
    operation: UserProfileOperation,
    code: UserProfileErrorCode,
    current_revision: Option<u64>,
) -> UserProfileResponse {
    UserProfileResponse::Error {
        version: USER_PROFILE_PROTOCOL_VERSION,
        error: UserProfileError {
            code,
            operation,
            current_revision,
            retry_after_ms: None,
        },
    }
}

fn user_profile_audit(
    id: String,
    boundary: &sylvander_protocol::BoundaryContext,
    owner: &UserId,
    operation: UserProfileOperation,
    outcome: &'static str,
    error_code: Option<String>,
) -> AdministrationAudit {
    AdministrationAudit {
        id,
        occurred_at: sylvander_agent::session::now_secs(),
        request_id: boundary.request_id.clone(),
        principal_digest: boundary.principal.as_ref().map_or_else(
            || sha256_text("unauthenticated"),
            |principal| sha256_text(&principal.id.0),
        ),
        channel_instance_id: boundary.channel_instance_id.clone(),
        transport: boundary.transport.clone(),
        operation: user_profile_operation_name(operation).into(),
        resource_kind: "user_profile".into(),
        resource_digest: sha256_text(&owner.0),
        version: None,
        outcome: outcome.into(),
        error_code,
    }
}

fn user_profile_outcome(response: &UserProfileResponse) -> (&'static str, Option<String>) {
    match response {
        UserProfileResponse::Error { error, .. } => {
            ("failed", Some(user_profile_error_name(error.code).into()))
        }
        _ => ("succeeded", None),
    }
}

const fn user_profile_operation_name(operation: UserProfileOperation) -> &'static str {
    match operation {
        UserProfileOperation::Create => "user_profile_create",
        UserProfileOperation::Read => "user_profile_read",
        UserProfileOperation::Update => "user_profile_update",
        UserProfileOperation::Export => "user_profile_export",
        UserProfileOperation::Correct => "user_profile_correct",
        UserProfileOperation::Delete => "user_profile_delete",
        UserProfileOperation::SetDoNotLearn => "user_profile_set_do_not_learn",
    }
}

const fn user_profile_error_name(code: UserProfileErrorCode) -> &'static str {
    match code {
        UserProfileErrorCode::UnsupportedVersion => "unsupported_version",
        UserProfileErrorCode::InvalidRequest => "invalid_request",
        UserProfileErrorCode::Unauthenticated => "unauthenticated",
        UserProfileErrorCode::Forbidden => "forbidden",
        UserProfileErrorCode::NotFound => "not_found",
        UserProfileErrorCode::AlreadyExists => "already_exists",
        UserProfileErrorCode::Conflict => "conflict",
        UserProfileErrorCode::RateLimited => "rate_limited",
        UserProfileErrorCode::ServiceUnavailable => "service_unavailable",
        UserProfileErrorCode::Internal => "internal",
    }
}

fn validate_external_metadata(
    boundary: &sylvander_protocol::BoundaryContext,
    metadata: &BTreeMap<String, String>,
) -> Result<(), sylvander_protocol::BoundaryError> {
    if metadata.len() > 32
        || metadata.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > 64
                || key.chars().any(char::is_control)
                || value.len() > 4096
                || value.chars().any(char::is_control)
        })
    {
        return Err(boundary_failure(
            boundary,
            "submit_chat",
            "external metadata exceeds the accepted shape",
        ));
    }
    Ok(())
}

fn agent_admin_error(code: AgentAdminErrorCode, message: impl Into<String>) -> AgentAdminResponse {
    AgentAdminResponse::Error {
        error: AgentAdminError {
            code,
            message: message.into(),
            agent_id: None,
            revision: None,
            expected_active_revision: None,
            actual_active_revision: None,
        },
    }
}

fn agent_admin_mutation(request: &AgentAdminRequest) -> Option<(&'static str, &AgentId, u64, u64)> {
    match request {
        AgentAdminRequest::UpdateDefinition {
            expected_active_revision,
            definition,
        } => Some((
            "update_definition",
            &definition.agent_id,
            definition.revision,
            *expected_active_revision,
        )),
        AgentAdminRequest::ActivateRevision {
            agent_id,
            revision,
            expected_active_revision,
        } => Some((
            "activate_revision",
            agent_id,
            *revision,
            *expected_active_revision,
        )),
        AgentAdminRequest::RollbackRevision {
            agent_id,
            target_revision,
            expected_active_revision,
        } => Some((
            "rollback_revision",
            agent_id,
            *target_revision,
            *expected_active_revision,
        )),
        AgentAdminRequest::InspectRevision { .. } | AgentAdminRequest::ListRevisions { .. } => None,
    }
}

fn agent_admin_error_code(code: AgentAdminErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "internal".into())
}

fn registry_admin_error(
    code: RegistryAdminErrorCode,
    message: &'static str,
) -> RegistryAdminResponse {
    RegistryAdminResponse::Error {
        error: RegistryAdminError {
            code,
            message: message.into(),
            provider_id: None,
            model_id: None,
            binding_id_sha256: None,
            revision: None,
            generation: None,
            details: None,
        },
    }
}

type RegistryAdministrationTarget = (&'static str, &'static str, String, Option<u64>);

fn registry_administration_audit(
    id: String,
    boundary: &sylvander_protocol::BoundaryContext,
    principal: &sylvander_protocol::AuthenticatedPrincipal,
    target: &RegistryAdministrationTarget,
    outcome: &'static str,
    error_code: Option<String>,
) -> AdministrationAudit {
    AdministrationAudit {
        id,
        occurred_at: sylvander_agent::session::now_secs(),
        request_id: boundary.request_id.clone(),
        principal_digest: sha256_text(&principal.id.0),
        channel_instance_id: boundary.channel_instance_id.clone(),
        transport: boundary.transport.clone(),
        operation: target.0.into(),
        resource_kind: target.1.into(),
        resource_digest: sha256_text(&target.2),
        version: target.3,
        outcome: outcome.into(),
        error_code,
    }
}

fn registry_admin_outcome(response: &RegistryAdminResponse) -> (&'static str, Option<String>) {
    match response {
        RegistryAdminResponse::Success { .. } => ("succeeded", None),
        RegistryAdminResponse::Error { error } => {
            ("failed", Some(registry_admin_error_code(error.code).into()))
        }
    }
}

const fn registry_admin_is_mutation(request: &RegistryAdminRequest) -> bool {
    matches!(
        request,
        RegistryAdminRequest::CreateProvider { .. }
            | RegistryAdminRequest::StageProviderRevision { .. }
            | RegistryAdminRequest::ActivateProviderRevision { .. }
            | RegistryAdminRequest::RollbackProviderRevision { .. }
            | RegistryAdminRequest::CreateModel { .. }
            | RegistryAdminRequest::StageModelRevision { .. }
            | RegistryAdminRequest::ActivateModelRevision { .. }
            | RegistryAdminRequest::RollbackModelRevision { .. }
            | RegistryAdminRequest::CreateCredentialBinding { .. }
            | RegistryAdminRequest::StageCredentialGeneration { .. }
            | RegistryAdminRequest::ActivateCredentialGeneration { .. }
            | RegistryAdminRequest::RollbackCredentialGeneration { .. }
    )
}

const fn registry_admin_is_credential_mutation(request: &RegistryAdminRequest) -> bool {
    matches!(
        request,
        RegistryAdminRequest::CreateCredentialBinding { .. }
            | RegistryAdminRequest::StageCredentialGeneration { .. }
            | RegistryAdminRequest::ActivateCredentialGeneration { .. }
            | RegistryAdminRequest::RollbackCredentialGeneration { .. }
    )
}

fn registry_admin_audit_target(request: &RegistryAdminRequest) -> RegistryAdministrationTarget {
    match request {
        RegistryAdminRequest::InspectProviderRevision {
            provider_id,
            revision,
        } => (
            "inspect_provider_revision",
            "provider",
            provider_id.clone(),
            Some(*revision),
        ),
        RegistryAdminRequest::ListProviderRevisions { provider_id, .. } => (
            "list_provider_revisions",
            "provider",
            provider_id.clone(),
            None,
        ),
        RegistryAdminRequest::CreateProvider { provider_id, .. } => {
            ("create_provider", "provider", provider_id.clone(), Some(1))
        }
        RegistryAdminRequest::StageProviderRevision {
            provider_id,
            revision,
            ..
        } => (
            "stage_provider_revision",
            "provider",
            provider_id.clone(),
            Some(*revision),
        ),
        RegistryAdminRequest::ActivateProviderRevision {
            provider_id,
            revision,
            ..
        } => (
            "activate_provider_revision",
            "provider",
            provider_id.clone(),
            Some(*revision),
        ),
        RegistryAdminRequest::RollbackProviderRevision {
            provider_id,
            target_revision,
            ..
        } => (
            "rollback_provider_revision",
            "provider",
            provider_id.clone(),
            Some(*target_revision),
        ),
        RegistryAdminRequest::InspectModelRevision {
            provider_id,
            model_id,
            revision,
        } => (
            "inspect_model_revision",
            "model",
            format!("{provider_id}/{model_id}"),
            Some(*revision),
        ),
        RegistryAdminRequest::ListModelRevisions {
            provider_id,
            model_id,
            ..
        } => (
            "list_model_revisions",
            "model",
            format!("{provider_id}/{model_id}"),
            None,
        ),
        RegistryAdminRequest::CreateModel {
            provider_id,
            model_id,
            ..
        } => (
            "create_model",
            "model",
            format!("{provider_id}/{model_id}"),
            Some(1),
        ),
        RegistryAdminRequest::StageModelRevision {
            provider_id,
            model_id,
            revision,
            ..
        } => (
            "stage_model_revision",
            "model",
            format!("{provider_id}/{model_id}"),
            Some(*revision),
        ),
        RegistryAdminRequest::ActivateModelRevision {
            provider_id,
            model_id,
            revision,
            ..
        } => (
            "activate_model_revision",
            "model",
            format!("{provider_id}/{model_id}"),
            Some(*revision),
        ),
        RegistryAdminRequest::RollbackModelRevision {
            provider_id,
            model_id,
            target_revision,
            ..
        } => (
            "rollback_model_revision",
            "model",
            format!("{provider_id}/{model_id}"),
            Some(*target_revision),
        ),
        RegistryAdminRequest::InspectCredentialGeneration {
            binding_id,
            generation,
        } => (
            "inspect_credential_generation",
            "credential",
            binding_id.clone(),
            Some(*generation),
        ),
        RegistryAdminRequest::ListCredentialGenerations { binding_id, .. } => (
            "list_credential_generations",
            "credential",
            binding_id.clone(),
            None,
        ),
        RegistryAdminRequest::CreateCredentialBinding { binding_id, .. } => (
            "create_credential_binding",
            "credential",
            binding_id.clone(),
            Some(1),
        ),
        RegistryAdminRequest::StageCredentialGeneration {
            binding_id,
            generation,
            ..
        } => (
            "stage_credential_generation",
            "credential",
            binding_id.clone(),
            Some(*generation),
        ),
        RegistryAdminRequest::ActivateCredentialGeneration {
            binding_id,
            generation,
            ..
        } => (
            "activate_credential_generation",
            "credential",
            binding_id.clone(),
            Some(*generation),
        ),
        RegistryAdminRequest::RollbackCredentialGeneration {
            binding_id,
            target_generation,
            ..
        } => (
            "rollback_credential_generation",
            "credential",
            binding_id.clone(),
            Some(*target_generation),
        ),
    }
}

const fn registry_admin_error_code(code: RegistryAdminErrorCode) -> &'static str {
    match code {
        RegistryAdminErrorCode::Unauthorized => "unauthorized",
        RegistryAdminErrorCode::InvalidRequest => "invalid_request",
        RegistryAdminErrorCode::UnknownProvider => "unknown_provider",
        RegistryAdminErrorCode::UnknownModel => "unknown_model",
        RegistryAdminErrorCode::UnknownCredentialBinding => "unknown_credential_binding",
        RegistryAdminErrorCode::UnknownRevision => "unknown_revision",
        RegistryAdminErrorCode::UnknownGeneration => "unknown_generation",
        RegistryAdminErrorCode::ProviderAlreadyExists => "provider_already_exists",
        RegistryAdminErrorCode::ModelAlreadyExists => "model_already_exists",
        RegistryAdminErrorCode::ActiveRevisionConflict => "active_revision_conflict",
        RegistryAdminErrorCode::NonSequentialRevision => "non_sequential_revision",
        RegistryAdminErrorCode::RevisionCollision => "revision_collision",
        RegistryAdminErrorCode::InvalidRevisionRollback => "invalid_revision_rollback",
        RegistryAdminErrorCode::CredentialAlreadyExists => "credential_already_exists",
        RegistryAdminErrorCode::ActiveGenerationConflict => "active_generation_conflict",
        RegistryAdminErrorCode::NonSequentialGeneration => "non_sequential_generation",
        RegistryAdminErrorCode::GenerationCollision => "generation_collision",
        RegistryAdminErrorCode::InvalidRollback => "invalid_rollback",
        RegistryAdminErrorCode::CredentialUnavailable => "credential_unavailable",
        RegistryAdminErrorCode::StorageUnavailable => "storage_unavailable",
        RegistryAdminErrorCode::IntegrityFailure => "integrity_failure",
        RegistryAdminErrorCode::Internal => "internal",
    }
}

fn ui_operation(message: &sylvander_protocol::UiClientMessage) -> &'static str {
    use sylvander_protocol::UiClientMessage as Message;
    match message {
        Message::Hello { .. } => "hello",
        Message::Chat { .. } => "chat",
        Message::Approve { .. } => "approve",
        Message::Answer { .. } => "answer",
        Message::Interrupt { .. } => "interrupt",
        Message::ResolvePlan { .. } => "resolve_plan",
        Message::CancelTask { .. } => "cancel_task",
        Message::DiscoverAgents => "discover_agents",
        Message::CreateSession { .. } => "create_session",
        Message::GetSessionConfig { .. } => "get_session_config",
        Message::UpdateSessionConfig { .. } => "update_session_config",
        Message::SubmitFeedback { .. } => "submit_feedback",
        Message::MemoryConfirmation { request } => request.operation(),
        Message::AgentAdmin { .. } => "agent_admin",
        Message::RegistryAdmin { .. } => "registry_admin",
        Message::UserProfile { .. } => "user_profile",
        Message::IdentityBinding { .. } => "identity_binding",
        Message::ListSessions => "list_sessions",
        Message::LoadSession { .. } => "load_session",
        Message::ReattachSession { .. } => "reattach_session",
        Message::RenameSession { .. } => "rename_session",
        Message::ArchiveSession { .. } => "archive_session",
        Message::RestoreSession { .. } => "restore_session",
        Message::DeleteSession { .. } => "delete_session",
        Message::ForkSession { .. } => "fork_session",
        Message::GetRuntimeInfo => "get_runtime_info",
        Message::GetContext { .. } => "get_context",
        Message::Compact { .. } => "compact",
        Message::PreviewWorkspaceRollback { .. } => "preview_workspace_rollback",
        Message::RollbackWorkspace { .. } => "rollback_workspace",
        Message::InspectCodingSession { .. } => "inspect_coding_session",
        Message::AcceptCodingSession { .. } => "accept_coding_session",
        Message::DiscardCodingSession { .. } => "discard_coding_session",
        Message::SelectModel { .. } => "select_model",
        Message::SelectPermissions { .. } => "select_permissions",
        Message::Ping => "ping",
    }
}

fn ui_session_id(message: &sylvander_protocol::UiClientMessage) -> Option<&str> {
    use sylvander_protocol::UiClientMessage as Message;
    match message {
        Message::Chat { session_id, .. }
        | Message::GetContext { session_id }
        | Message::SelectModel { session_id, .. }
        | Message::SelectPermissions { session_id, .. } => session_id.as_deref(),
        Message::Approve { session_id, .. }
        | Message::Answer { session_id, .. }
        | Message::Interrupt { session_id }
        | Message::ResolvePlan { session_id, .. }
        | Message::CancelTask { session_id, .. }
        | Message::GetSessionConfig { session_id }
        | Message::LoadSession { session_id }
        | Message::ReattachSession { session_id }
        | Message::RenameSession { session_id, .. }
        | Message::ArchiveSession { session_id }
        | Message::RestoreSession { session_id }
        | Message::DeleteSession { session_id }
        | Message::ForkSession { session_id, .. }
        | Message::Compact { session_id }
        | Message::PreviewWorkspaceRollback { session_id }
        | Message::RollbackWorkspace { session_id, .. }
        | Message::InspectCodingSession { session_id }
        | Message::AcceptCodingSession { session_id }
        | Message::DiscardCodingSession { session_id } => Some(session_id),
        Message::UpdateSessionConfig { request } => Some(&request.session_id.0),
        Message::MemoryConfirmation { request } => Some(request.session_id()),
        _ => None,
    }
}

struct ChannelExitSignal {
    name: String,
    sender: tokio::sync::mpsc::UnboundedSender<String>,
}

impl Drop for ChannelExitSignal {
    fn drop(&mut self) {
        let _ = self.sender.send(self.name.clone());
    }
}

impl Runtime {
    /// Bootstrap an isolated in-memory Runtime for unit tests.
    ///
    /// # Flow
    ///
    /// 1. Create an in-process message bus
    /// 2. Create the engine
    /// 3. Create an in-memory session store
    /// 4. Load persistent sessions → re-create in engine
    /// 5. Spawn each agent from config
    /// 6. Create sessions defined in config
    #[cfg(test)]
    pub(crate) async fn boot(
        config: SystemConfig,
        default_client: AnthropicClient,
    ) -> Result<Self, RuntimeError> {
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = Arc::new(AgentRunEngine::new(bus.clone()));
        let session_store: Arc<dyn SessionStore> = Arc::new(
            SqliteSessionStore::open_in_memory()
                .await
                .map_err(|e| RuntimeError::Store(format!("open session store: {e}")))?,
        );
        let memory_store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());

        // Spawn agents
        for spec in &config.agents {
            let provider_id = spec.model.provider.clone();
            let model = spec
                .to_model_info()
                .map_err(|error| RuntimeError::Engine(error.to_string()))?;
            let exact = ProviderModelInfo {
                reference: ModelRef::new(&provider_id, model.id),
                context_window: model.context_window,
                max_output_tokens: model.max_output_tokens,
                capabilities: ProviderModelCapabilities::TOOL_USE,
            };
            let run = AgentRun::qualified_router_builder(
                spec.clone(),
                Arc::new(AnthropicProvider::new(provider_id, default_client.clone())),
                exact,
            )
            .bus(bus.clone())
            .session_store(session_store.clone())
            .memory(memory_store.clone())
            .override_tools(default_tools(memory_store.clone()))
            .build()
            .map_err(|error| RuntimeError::Engine(format!("build {} failed: {error}", spec.id)))?;
            engine
                .spawn_run(spec.clone(), run)
                .await
                .map_err(|e| RuntimeError::Engine(format!("spawn {} failed: {e}", spec.id)))?;
        }

        // Restore durable identities only after Agents subscribe to the bus.
        for session in session_store
            .list_persistent()
            .await
            .map_err(|e| RuntimeError::Store(format!("list persistent failed: {e}")))?
        {
            engine
                .attach_session(
                    session.id.clone(),
                    &session.name,
                    session.metadata.clone(),
                    &session.agents,
                )
                .await
                .map_err(|e| RuntimeError::Engine(format!("load session failed: {e}")))?;
        }

        // Create sessions from config
        for session in &config.sessions {
            engine
                .attach_session(
                    session.id.clone(),
                    &session.name,
                    session.metadata.clone(),
                    &session.agents,
                )
                .await
                .map_err(|e| {
                    RuntimeError::Engine(format!("create session {} failed: {e}", session.id))
                })?;
            if session.lifetime == SessionLifetime::Persistent {
                session_store
                    .save(session)
                    .await
                    .map_err(|e| RuntimeError::Store(format!("save session failed: {e}")))?;
            }
        }

        info!(name = %config.name, agents = config.agents.len(), "runtime booted");

        let (channel_exit_tx, channel_exits) = tokio::sync::mpsc::unbounded_channel();
        let configured_agents = HashMap::new();
        let ui_service = Arc::new(RuntimeUiService {
            engine: engine.clone(),
            bus: bus.clone(),
            sessions: session_store.clone(),
            agents: configured_agents.clone(),
            agent_registry: None,
            revision_provider: None,
            credential_resolver: None,
            credential_audit: None,
            evidence: None,
            evidence_run_id: None,
            guardian: None,
            identity_bindings: None,
            user_profiles: None,
            worktrees: None,
            boundary: BoundaryGuard::new(crate::config::BoundarySettings::default()),
        });
        Ok(Self {
            engine,
            session_store,
            memory_store,
            bus,
            configured_agents,
            revision_provider: None,
            ui_service,
            evidence: None,
            credential_audit: None,
            guardian: None,
            memory_maintenance: None,
            channels: tokio::sync::Mutex::new(Vec::new()),
            channel_exit_tx,
            channel_exits: tokio::sync::Mutex::new(channel_exits),
        })
    }

    /// Bootstrap the production runtime from validated server configuration.
    pub async fn boot_config(config: ServerConfig) -> Result<Self, RuntimeError> {
        Self::boot_config_with_provider_sources(config, None).await
    }

    /// Bootstrap with an injected renewable Provider credential service.
    ///
    /// The injected source is used by every initial and lazily recomposed
    /// Agent revision. Channel credentials remain independently owned by the
    /// server composition root.
    pub async fn boot_config_with_provider_credentials(
        config: ServerConfig,
        sources: ProviderCredentialSources,
    ) -> Result<Self, RuntimeError> {
        Self::boot_config_with_provider_sources(config, Some(sources)).await
    }

    async fn boot_config_with_provider_sources(
        config: ServerConfig,
        sources: Option<ProviderCredentialSources>,
    ) -> Result<Self, RuntimeError> {
        config
            .validate()
            .map_err(|error| RuntimeError::Config(error.to_string()))?;
        let mut config = with_resolved_paths(config)?;
        let worktrees = Arc::new(build_coding_worktree_service(&config)?);
        let session_db = config
            .server
            .session_db
            .as_ref()
            .expect("resolved session database");
        let memory_db = config
            .server
            .memory_db
            .as_ref()
            .expect("resolved memory database")
            .clone();
        let user_profile_db = config
            .server
            .user_profile_db
            .as_ref()
            .expect("resolved user profile database")
            .clone();
        if let Some(parent) = session_db.parent() {
            std::fs::create_dir_all(parent).map_err(|error| RuntimeError::Io {
                operation: "create session database directory",
                path: parent.display().to_string(),
                message: error.to_string(),
            })?;
        }
        if let Some(parent) = memory_db.parent() {
            std::fs::create_dir_all(parent).map_err(|error| RuntimeError::Io {
                operation: "create memory database directory",
                path: parent.display().to_string(),
                message: error.to_string(),
            })?;
        }
        if let Some(parent) = user_profile_db.parent() {
            std::fs::create_dir_all(parent).map_err(|error| RuntimeError::Io {
                operation: "create user profile database directory",
                path: parent.display().to_string(),
                message: error.to_string(),
            })?;
        }

        let user_profiles = UserProfileStore::open(&user_profile_db)
            .await
            .map_err(|_| RuntimeError::Store("open user profile store failed".into()))?;
        let credential_audit = Arc::new(
            CredentialOperationAuditLedger::open(
                config
                    .server
                    .data_dir
                    .as_ref()
                    .expect("resolved runtime data directory")
                    .join("credential-operations.db"),
            )
            .await
            .map_err(|_| RuntimeError::Store("open credential audit ledger failed".into()))?,
        );

        let session_store: Arc<dyn SessionStore> = Arc::new(
            SqliteSessionStore::open_shared(session_db, REGISTRY_SCHEMA_OBJECT_NAMES)
                .await
                .map_err(|error| RuntimeError::Store(error.to_string()))?,
        );
        let agent_registry = AgentRegistry::open_shared(session_db, SESSION_SCHEMA_OBJECT_NAMES)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?;
        let (credential_resolver, external_secret_provider) = sources.map_or_else(
            || {
                (
                    Arc::new(SystemSecretResolver) as Arc<dyn CredentialSecretResolver>,
                    None,
                )
            },
            |sources| (sources.resolver, Some(sources.lease_provider)),
        );
        agent_registry
            .bootstrap_registries(&config)
            .await
            .map_err(|error| RuntimeError::Config(error.to_string()))?;
        agent_registry
            .seed(&config)
            .await
            .map_err(|error| RuntimeError::Config(error.to_string()))?;
        let mut active_definitions = Vec::with_capacity(config.agents.len());
        for configured in &config.agents {
            let active = agent_registry
                .load_active(&configured.spec.id)
                .await
                .map_err(|error| RuntimeError::Store(error.to_string()))?
                .ok_or_else(|| {
                    RuntimeError::Config(format!(
                        "Agent {} has no active registry revision",
                        configured.spec.id
                    ))
                })?;
            active_definitions.push(active.definition);
        }
        for definition in &active_definitions {
            config
                .validate_agent_shape_and_environment(definition)
                .map_err(|error| {
                    RuntimeError::Config(format!("active Agent registry is incompatible: {error}"))
                })?;
        }
        config.agents = active_definitions;
        for definition in &config.agents {
            let existing = agent_registry
                .load_agent_snapshot_versioned(&definition.spec.id.0, definition.revision)
                .await
                .map_err(|error| RuntimeError::Composition(error.to_string()))?;
            if existing.is_none() {
                let selection = active_snapshot_selection(definition);
                agent_registry
                    .stage_agent_snapshot_v3(selection)
                    .await
                    .map_err(|error| RuntimeError::Composition(error.to_string()))?;
            }
        }

        let memory_policy =
            RuntimeMemoryMaintenancePolicy::from_settings(&config.server.memory_maintenance)?;
        let retention_policy = memory_policy.retention.clone();
        let integrity_settings = &config.server.memory_maintenance.integrity;
        let sqlite_memory = if config.server.mode == ServerMode::SelfUse
            && integrity_settings.key.is_none()
            && integrity_settings.backend.is_none()
        {
            tokio::task::spawn_blocking(move || {
                SqliteMemoryStore::open_with_retention_policy(memory_db, retention_policy)
            })
            .await
            .map_err(|error| RuntimeError::Store(format!("open memory store: {error}")))?
            .map_err(|error| RuntimeError::Store(error.to_string()))?
        } else {
            let integrity_secret = SystemSecretResolver
                .resolve(integrity_settings.key.as_ref().ok_or_else(|| {
                    RuntimeError::Config("memory integrity key reference is required".into())
                })?)
                .map_err(|_| {
                    RuntimeError::Config("memory integrity key resolution failed".into())
                })?;
            let integrity = match integrity_settings.backend.as_ref().ok_or_else(|| {
                RuntimeError::Config("memory integrity backend is required".into())
            })? {
                MemoryIntegrityBackend::File { anchor_path } => {
                    MemoryIntegrityConfig::new(anchor_path, integrity_secret.as_bytes())
                }
                MemoryIntegrityBackend::Http {
                    endpoint,
                    bearer_token,
                    ca_certificate,
                    client_identity,
                    timeout_millis,
                    read_retries,
                } => {
                    let bearer = SystemSecretResolver.resolve(bearer_token).map_err(|_| {
                        RuntimeError::Config(
                            "memory integrity HTTP bearer token resolution failed".into(),
                        )
                    })?;
                    let mut remote = HttpMemoryIntegrityAnchorConfig::new(
                        endpoint,
                        bearer.as_bytes(),
                        std::time::Duration::from_millis(u64::from(*timeout_millis)),
                        *read_retries,
                    )
                    .map_err(|_| {
                        RuntimeError::Config("memory integrity HTTP configuration failed".into())
                    })?;
                    let ca = ca_certificate
                        .as_ref()
                        .map(|reference| SystemSecretResolver.resolve(reference))
                        .transpose()
                        .map_err(|_| {
                            RuntimeError::Config(
                                "memory integrity HTTP CA resolution failed".into(),
                            )
                        })?;
                    if let Some(ca) = &ca {
                        remote = remote.with_ca_certificate(ca.as_bytes()).map_err(|_| {
                            RuntimeError::Config(
                                "memory integrity HTTP CA configuration failed".into(),
                            )
                        })?;
                    }
                    let identity = client_identity
                        .as_ref()
                        .map(|reference| SystemSecretResolver.resolve(reference))
                        .transpose()
                        .map_err(|_| {
                            RuntimeError::Config(
                                "memory integrity HTTP client identity resolution failed".into(),
                            )
                        })?;
                    if let Some(identity) = &identity {
                        remote =
                            remote
                                .with_client_identity(identity.as_bytes())
                                .map_err(|_| {
                                    RuntimeError::Config(
                                    "memory integrity HTTP client identity configuration failed"
                                        .into(),
                                )
                                })?;
                    }
                    let anchor = HttpMemoryIntegrityAnchor::new(remote).map_err(|_| {
                        RuntimeError::Config("memory integrity HTTP configuration failed".into())
                    })?;
                    MemoryIntegrityConfig::with_anchor(
                        Arc::new(anchor),
                        integrity_secret.as_bytes(),
                    )
                }
            }
            .map_err(|_| RuntimeError::Config("memory integrity configuration failed".into()))?;
            tokio::task::spawn_blocking(move || {
                SqliteMemoryStore::open_with_integrity(memory_db, retention_policy, integrity)
            })
            .await
            .map_err(|error| RuntimeError::Store(format!("open memory store: {error}")))?
            .map_err(|error| RuntimeError::Store(error.to_string()))?
        };
        let memory_maintenance_handle = sqlite_memory.maintenance();
        let memory_store: Arc<dyn MemoryStore> = Arc::new(sqlite_memory);
        let bus = Arc::new(InProcessMessageBus::new());
        let engine = Arc::new(AgentRunEngine::new(bus.clone()));
        let evidence_path = config
            .server
            .evidence
            .path
            .as_ref()
            .expect("resolved security audit path");
        if let Some(parent) = evidence_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| RuntimeError::Io {
                operation: "create evidence directory",
                path: parent.display().to_string(),
                message: error.to_string(),
            })?;
        }
        // Security denials and runtime facts share one always-on durable
        // evidence boundary. Content policy controls payload capture; it
        // never disables the operational record.
        let security_audit = if let Some(encryption) = &config.server.evidence.encryption {
            let secret = SystemSecretResolver
                .resolve(&encryption.key)
                .map_err(|_| RuntimeError::Config("evidence encryption key unavailable".into()))?;
            let encryption =
                EvidenceEncryption::from_secret(encryption.key_id.clone(), secret.as_bytes())
                    .map_err(|error| RuntimeError::Config(error.to_string()))?;
            let governance = EvidenceGovernance::new(
                config.server.evidence.tenant_id.clone(),
                config.server.evidence.retention_days,
                encryption,
            )
            .map_err(|error| RuntimeError::Config(error.to_string()))?;
            EvidenceStore::open_governed(evidence_path, governance).await
        } else {
            EvidenceStore::open(evidence_path).await
        }
        .map_err(|error| RuntimeError::Evidence(error.to_string()))?;
        let result_artifacts: Option<Arc<dyn McpResultArtifactSink>> =
            if security_audit.governance_enabled() {
                Some(Arc::new(
                    EvidenceArtifactSink::new(security_audit.clone())
                        .map_err(|error| RuntimeError::Evidence(error.to_string()))?,
                ))
            } else {
                None
            };
        let identity_bindings = open_identity_binding_service(&config).await?;
        let evidence = Some(
            EvidenceRecorder::start(
                bus.clone(),
                security_audit.clone(),
                config.server.name.clone(),
                config.server.evidence.content,
                config.server.evidence.retention_days,
            )
            .await
            .map_err(|error| RuntimeError::Evidence(error.to_string()))?,
        );
        let guardian_now = sylvander_agent::session::now_secs();
        let guardian_settings = GuardianRuntimeSettings::for_runtime(
            config
                .server
                .data_dir
                .as_deref()
                .expect("resolved runtime data directory"),
            &config.server.name,
            guardian_now,
        );
        let tool_gateway_factory =
            WorkerToolGatewayFactory::open(&guardian_settings, guardian_now, user_profiles.clone())
                .await
                .map_err(|error| RuntimeError::Store(error.to_string()))?;
        let mut agents = Vec::with_capacity(config.agents.len());
        for definition in &config.agents {
            let snapshot = agent_registry
                .resolve_registry_composition_versioned(&definition.spec.id, definition.revision)
                .await
                .map_err(|error| RuntimeError::Composition(error.to_string()))?;
            agents.push(
                build_registry_agent_versioned_with_resolver(
                    &config,
                    snapshot,
                    agent_registry.clone(),
                    bus.clone(),
                    session_store.clone(),
                    memory_store.clone(),
                    Some(Arc::new(user_profiles.clone())),
                    credential_resolver.clone(),
                    external_secret_provider.clone(),
                    credential_audit.clone(),
                    result_artifacts.clone(),
                    Some(tool_gateway_factory.clone()),
                )
                .await
                .map_err(|error| RuntimeError::Composition(error.to_string()))?,
            );
        }
        let mut configured_agents = HashMap::new();
        for agent in agents {
            configured_agents.insert(agent.spec.id.clone(), agent);
        }
        let revision_provider = Arc::new(RuntimeRevisionProvider {
            config: config.clone(),
            registry: agent_registry.clone(),
            bus: bus.clone(),
            sessions: session_store.clone(),
            memory: memory_store.clone(),
            user_profiles: Arc::new(user_profiles.clone()),
            credential_resolver: credential_resolver.clone(),
            external_secret_provider,
            credential_audit: credential_audit.clone(),
            result_artifacts,
            tool_gateway_factory,
            configured: RwLock::new(
                configured_agents
                    .values()
                    .map(|agent| {
                        (
                            (agent.spec.id.clone(), agent.definition.revision),
                            agent.clone(),
                        )
                    })
                    .collect(),
            ),
        });
        for agent in configured_agents.values() {
            engine
                .spawn_revisioned_run(
                    agent.spec.clone(),
                    agent.definition.revision,
                    agent.run.clone(),
                    revision_provider.clone(),
                )
                .await
                .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        }

        let persistent_sessions = session_store
            .list_persistent()
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?;
        let active_worktrees = persistent_sessions
            .iter()
            .filter(|session| session.external_meta.contains_key("git_worktree"))
            .map(|session| {
                Ok(coding_worktree::ActiveCodingWorkspace {
                    session_id: session.id.0.clone(),
                    effective_workspace: session.metadata.workspace.clone(),
                    target_id: coding_worktree_target(session)?.map(str::to_owned),
                })
            })
            .collect::<Result<Vec<_>, String>>()
            .map_err(RuntimeError::Store)?;
        let reconciliation = worktrees
            .reconcile(&active_worktrees)
            .await
            .map_err(|error| RuntimeError::Store(format!("worktree reconciliation: {error}")))?;
        info!(
            retained = reconciliation.retained,
            removed = reconciliation.removed,
            "reconciled coding session worktrees"
        );

        for mut session in persistent_sessions {
            if session.agents.len() != 1 {
                return Err(RuntimeError::Config(format!(
                    "revisioned session {} requires exactly one Agent",
                    session.id
                )));
            }
            let agent = session
                .agents
                .iter()
                .find_map(|id| configured_agents.get(id))
                .ok_or_else(|| {
                    RuntimeError::Config(format!("session {} has no configured Agent", session.id))
                })?;
            let closure = close_session_revision_pins(&agent_registry, &session, agent).await?;
            if closure.changed {
                session.config_revision = session_store
                    .update_config(
                        &session.id,
                        session.config_revision,
                        session.config_overrides.clone(),
                        closure.effective.clone(),
                    )
                    .await
                    .map_err(|error| RuntimeError::Store(error.to_string()))?;
            }
            session.effective_config = Some(closure.effective);
            agent
                .attach_authenticated_session(session.id.clone(), session.metadata.clone())
                .await
                .map_err(|error| RuntimeError::Engine(error.to_string()))?;
            engine
                .attach_session(
                    session.id.clone(),
                    &session.name,
                    session.metadata,
                    &session.agents,
                )
                .await
                .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        }

        // The protected store stages the configured policy at open but keeps
        // the prior active policy authoritative. Complete every fallible
        // Agent, session, evidence, and maintenance readiness check first.
        // Only then may this rollout advance the durable policy revision.
        memory_maintenance_catch_up(&memory_maintenance_handle, &memory_policy).await?;
        let activation = memory_maintenance_handle.clone();
        tokio::task::spawn_blocking(move || activation.activate_staged_retention_policy())
            .await
            .map_err(|_| RuntimeError::Store("memory retention activation failed".into()))?
            .map_err(|_| RuntimeError::Store("memory retention activation failed".into()))?;
        let guardian = Arc::new(
            GuardianRuntime::start(
                guardian_settings,
                guardian_now,
                Arc::new(user_profiles.clone()),
            )
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?,
        );
        for session in session_store
            .list_persistent()
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
        {
            guardian
                .audit_worker_session_binding(&session, guardian_now)
                .await
                .map_err(|error| RuntimeError::Store(error.to_string()))?;
        }
        guardian
            .drain_once(guardian_now)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?;

        info!(
            name = %config.server.name,
            agents = configured_agents.len(),
            session_db = %session_db.display(),
            "configured runtime booted"
        );
        let ui_service = Arc::new(RuntimeUiService {
            engine: engine.clone(),
            bus: bus.clone(),
            sessions: session_store.clone(),
            agents: configured_agents.clone(),
            agent_registry: Some(agent_registry.clone()),
            revision_provider: Some(revision_provider.clone()),
            credential_resolver: Some(credential_resolver),
            credential_audit: Some(credential_audit.clone()),
            evidence: Some(security_audit),
            evidence_run_id: evidence
                .as_ref()
                .map(|recorder| recorder.run_id().to_string()),
            guardian: Some(guardian.clone()),
            identity_bindings,
            user_profiles: Some(user_profiles),
            worktrees: Some(worktrees),
            boundary: BoundaryGuard::new(config.server.boundary.clone()),
        });
        let (channel_exit_tx, channel_exits) = tokio::sync::mpsc::unbounded_channel();
        let memory_maintenance = Some(MemoryMaintenanceTask::start(
            memory_maintenance_handle,
            memory_policy,
            config
                .server
                .data_dir
                .clone()
                .expect("resolved runtime data directory"),
        ));
        Ok(Self {
            engine,
            session_store,
            memory_store,
            bus,
            configured_agents,
            revision_provider: Some(revision_provider),
            ui_service,
            evidence,
            credential_audit: Some(credential_audit),
            guardian: Some(guardian),
            memory_maintenance,
            channels: tokio::sync::Mutex::new(Vec::new()),
            channel_exit_tx,
            channel_exits: tokio::sync::Mutex::new(channel_exits),
        })
    }

    /// Return redacted transport metadata for one configured Agent.
    #[must_use]
    pub fn agent_descriptor(&self, id: &AgentId) -> Option<composition::ConfiguredAgentDescriptor> {
        self.configured_agents
            .get(id)
            .map(ConfiguredAgent::descriptor)
    }

    /// Return the Runtime-owned credential operation ledger for Channel
    /// composition. Legacy in-memory bootstrap intentionally has no durable
    /// ledger and therefore returns `None`.
    #[must_use]
    pub fn credential_audit_ledger(&self) -> Option<Arc<CredentialOperationAuditLedger>> {
        self.credential_audit.clone()
    }

    /// Inspect all tracked and untracked changes in an isolated coding session.
    pub async fn inspect_coding_session(
        &self,
        session_id: &SessionId,
    ) -> Result<git_worktree::WorkspaceDiff, RuntimeError> {
        let session = self
            .session_store
            .get(session_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .ok_or_else(|| RuntimeError::Store(format!("unknown session {session_id}")))?;
        let manager = self
            .ui_service
            .worktrees
            .clone()
            .ok_or_else(|| RuntimeError::Engine("coding worktrees are unavailable".into()))?;
        let target = coding_worktree_target(&session).map_err(RuntimeError::Engine)?;
        manager
            .inspect(&session_id.0, target)
            .await
            .map_err(RuntimeError::Engine)
    }

    /// Merge the reviewed coding-session changes while keeping the session open.
    pub async fn accept_coding_session(&self, session_id: &SessionId) -> Result<(), RuntimeError> {
        let session = self
            .session_store
            .get(session_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .ok_or_else(|| RuntimeError::Store(format!("unknown session {session_id}")))?;
        let manager = self
            .ui_service
            .worktrees
            .clone()
            .ok_or_else(|| RuntimeError::Engine("coding worktrees are unavailable".into()))?;
        let target = coding_worktree_target(&session).map_err(RuntimeError::Engine)?;
        manager
            .accept(&session_id.0, target)
            .await
            .map_err(RuntimeError::Engine)
    }

    /// Abandon an isolated coding session and remove its worktree.
    pub async fn discard_coding_session(&self, session_id: &SessionId) -> Result<(), RuntimeError> {
        let session = self
            .session_store
            .get(session_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .ok_or_else(|| RuntimeError::Store(format!("unknown session {session_id}")))?;
        let manager = self
            .ui_service
            .worktrees
            .clone()
            .ok_or_else(|| RuntimeError::Engine("coding worktrees are unavailable".into()))?;
        let target = coding_worktree_target(&session).map_err(RuntimeError::Engine)?;
        manager
            .discard(&session_id.0, target)
            .await
            .map_err(RuntimeError::Engine)?;
        self.engine.detach_session(session_id).await;
        for agent_id in &session.agents {
            if let Some(agent) = self.configured_agents.get(agent_id) {
                agent.detach_authenticated_session(session_id).await;
            }
        }
        self.session_store
            .delete(session_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?;
        if let Some(guardian) = &self.guardian
            && let Err(error) = guardian
                .enqueue_session_closed(&session, sylvander_agent::session::now_secs())
                .await
        {
            warn!(
                %error,
                %session_id,
                "failed to enqueue closed session for Guardian curation"
            );
        }
        Ok(())
    }

    #[cfg(test)]
    fn configured_agent(&self, id: &AgentId) -> Option<&ConfiguredAgent> {
        self.configured_agents.get(id)
    }

    /// Resolve and atomically replace one durable session's sparse overrides.
    /// The expected revision prevents two clients from silently overwriting
    /// each other's model, prompt, permission, or workspace choices.
    pub async fn update_session_config(
        &self,
        session_id: &SessionId,
        expected_revision: u64,
        overrides: SessionConfigOverrides,
    ) -> Result<(u64, SessionEffectiveConfig), RuntimeError> {
        let session = self
            .session_store
            .get(session_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .ok_or_else(|| RuntimeError::Config(format!("unknown session {session_id}")))?;
        let agent = if let (Some(provider), Some(effective)) =
            (&self.revision_provider, &session.effective_config)
        {
            provider
                .configured_revision(&effective.agent_id, effective.agent_revision)
                .await?
        } else {
            session
                .agents
                .iter()
                .find_map(|id| self.configured_agents.get(id))
                .cloned()
                .ok_or_else(|| {
                    RuntimeError::Config(format!("session {session_id} has no configured Agent"))
                })?
        };
        ensure_workspace_update_is_static(&session, &overrides).map_err(RuntimeError::Config)?;
        let mut effective =
            resolve_session_config(&agent, &overrides, None, None).map_err(|error| {
                RuntimeError::Config(format!(
                    "resolve configuration for session {session_id}: {error}"
                ))
            })?;
        bind_effective_workspace(&mut effective, &session.metadata.workspace);
        let revision = self
            .session_store
            .update_config(session_id, expected_revision, overrides, effective.clone())
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?;
        Ok((revision, effective))
    }

    /// Return the shared message bus used by protocol adapters.
    #[must_use]
    pub fn bus(&self) -> Arc<dyn MessageBus> {
        self.bus.clone()
    }

    /// Return the durable evidence store when collection is enabled.
    #[must_use]
    pub fn evidence_store(&self) -> Option<EvidenceStore> {
        self.ui_service.evidence.clone()
    }

    // -- channels --

    /// Start protocol channels. Each runs in its own tokio task.
    pub async fn start_channels(
        &self,
        channels: Vec<ChannelRegistration>,
    ) -> Result<(), RuntimeError> {
        let mut tasks = self.channels.lock().await;
        if !tasks.is_empty() {
            return Err(RuntimeError::Channel(
                "channels have already been started".into(),
            ));
        }
        let mut instance_ids = BTreeSet::new();
        for registration in &channels {
            if registration.instance_id.trim().is_empty() {
                return Err(RuntimeError::Channel(
                    "channel instance id cannot be empty".into(),
                ));
            }
            if !instance_ids.insert(registration.instance_id.clone()) {
                return Err(RuntimeError::Channel(format!(
                    "duplicate channel instance id {}",
                    registration.instance_id
                )));
            }
        }
        for registration in channels {
            let lifecycle = ChannelReadiness::new();
            let instance_id = registration.instance_id;
            let kind = registration.channel.name().to_string();
            let health = Arc::new(std::sync::RwLock::new(ChannelHealth {
                instance_id: instance_id.clone(),
                kind: kind.clone(),
                status: ChannelStatus::Starting,
                restart_count: 0,
            }));
            let task_instance_id = instance_id.clone();
            let exit_tx = self.channel_exit_tx.clone();
            let task_lifecycle = lifecycle.clone();
            let task_health = health.clone();
            let bus = self.bus.clone();
            let sessions = self.session_store.clone();
            let ui = self.ui_service.clone();
            let mut task = tokio::spawn(async move {
                let _exit_signal = ChannelExitSignal {
                    name: task_instance_id,
                    sender: exit_tx,
                };
                supervise_channel(
                    registration.channel,
                    registration.restart,
                    registration.session_defaults,
                    (bus, sessions, ui),
                    task_lifecycle,
                    task_health,
                )
                .await;
            });
            let startup = tokio::select! {
                result = &mut task => {
                    Err(RuntimeError::Channel(match result {
                        Ok(()) => format!("channel {instance_id} exited before becoming ready"),
                        Err(error) => format!("channel {instance_id} failed during startup: {error}"),
                    }))
                }
                result = tokio::time::timeout(
                    tokio::time::Duration::from_secs(5),
                    lifecycle.wait(),
                ) => {
                    if result.is_err() {
                        lifecycle.request_shutdown();
                        task.abort();
                        let _ = (&mut task).await;
                        Err(RuntimeError::Channel(format!(
                            "channel {instance_id} did not become ready within 5 seconds"
                        )))
                    } else {
                        Ok(())
                    }
                }
            };
            if let Err(error) = startup {
                let started = tasks.drain(..).collect();
                if let Err(shutdown_error) = stop_channel_tasks(started).await {
                    warn!(%shutdown_error, "failed to roll back channels after startup failure");
                }
                return Err(error);
            }
            info!(instance = %instance_id, kind = %kind, "channel ready");
            tasks.push(ChannelTask {
                instance_id,
                task,
                lifecycle,
                health,
            });
        }
        Ok(())
    }

    /// Return a stable snapshot of every supervised channel instance.
    pub async fn channel_health(&self) -> Vec<ChannelHealth> {
        let tasks = self.channels.lock().await;
        tasks
            .iter()
            .filter_map(|task| task.health.read().ok().map(|health| health.clone()))
            .collect()
    }

    /// Return one content-safe operational snapshot for health endpoints,
    /// diagnostics, metrics exporters, and alerting adapters.
    pub async fn operational_snapshot(&self) -> Result<RuntimeOperationalSnapshot, RuntimeError> {
        let agent_count = self.engine.list_agents().await.len();
        let persistent_session_count = self
            .session_store
            .list_persistent()
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .len();
        let channels = self.channel_health().await;
        let evidence = if let Some(recorder) = &self.evidence {
            Some(
                recorder
                    .store()
                    .counts()
                    .await
                    .map_err(|error| RuntimeError::Evidence(error.to_string()))?,
            )
        } else {
            None
        };
        let mut health_issues = Vec::new();
        if let Some(recorder) = &self.evidence
            && recorder.last_error().await.is_some()
        {
            health_issues.push(RuntimeHealthIssue::EvidenceRecorder);
        }
        if let Some(guardian) = &self.guardian
            && guardian.last_error().await.is_some()
        {
            health_issues.push(RuntimeHealthIssue::GuardianSupervisor);
        }
        let ready = agent_count == self.configured_agents.len()
            && channels
                .iter()
                .all(|channel| channel.status == ChannelStatus::Ready)
            && health_issues.is_empty();
        Ok(RuntimeOperationalSnapshot {
            ready,
            agent_count,
            persistent_session_count,
            channels,
            bus: self.bus.diagnostics().await,
            evidence,
            health_issues,
        })
    }

    /// Wait until a started channel exits unexpectedly.
    pub async fn wait_for_channel_exit(&self) -> Option<String> {
        self.channel_exits.lock().await.recv().await
    }

    /// Wait until an Agent task exits without a matching shutdown request.
    pub async fn wait_for_agent_exit(&self) -> Option<AgentId> {
        self.engine.wait_for_agent_exit().await
    }

    // -- shutdown --

    /// Graceful shutdown — despawn all agents.
    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        // Stop accepting external work before stopping the Agents that serve it.
        let channel_tasks = {
            let mut tasks = self.channels.lock().await;
            tasks.drain(..).collect::<Vec<_>>()
        };
        let mut first_error = stop_channel_tasks(channel_tasks).await.err();
        let agents = self.engine.list_agents().await;
        for handle in agents {
            if let Err(error) = self.engine.despawn(&handle.id).await {
                first_error.get_or_insert_with(|| {
                    RuntimeError::Engine(format!("despawn {} failed: {error}", handle.id))
                });
            }
        }
        if let Some(guardian) = &self.guardian
            && let Err(error) = guardian.shutdown().await
        {
            first_error.get_or_insert_with(|| RuntimeError::Store(error.to_string()));
        }
        if let Some(guardian) = &self.guardian
            && let Some(error) = guardian.last_error().await
        {
            warn!(%error, "Guardian supervisor stopped after a recorded error");
        }
        if let Some(evidence) = &self.evidence
            && let Err(error) = evidence.shutdown().await
        {
            first_error.get_or_insert_with(|| RuntimeError::Evidence(error.to_string()));
        }
        if let Some(maintenance) = &self.memory_maintenance {
            maintenance.shutdown().await;
        }
        info!("runtime shut down");
        first_error.map_or(Ok(()), Err)
    }
}

async fn stop_channel_tasks(channel_tasks: Vec<ChannelTask>) -> Result<(), RuntimeError> {
    for channel in &channel_tasks {
        channel.lifecycle.request_shutdown();
    }
    let mut first_error = None;
    for mut channel in channel_tasks {
        let result =
            tokio::time::timeout(tokio::time::Duration::from_secs(5), &mut channel.task).await;
        let result = if let Ok(result) = result {
            result
        } else {
            warn!(instance = %channel.instance_id, "channel drain timed out; aborting task");
            channel.task.abort();
            channel.task.await
        };
        match result {
            Ok(()) => info!(instance = %channel.instance_id, "channel stopped"),
            Err(error) if error.is_cancelled() => {
                info!(instance = %channel.instance_id, "channel cancelled during shutdown");
            }
            Err(error) => {
                first_error.get_or_insert_with(|| {
                    RuntimeError::Channel(format!(
                        "channel {} task failed: {error}",
                        channel.instance_id
                    ))
                });
            }
        }
    }
    if let Some(error) = first_error {
        Err(error)
    } else {
        Ok(())
    }
}

async fn supervise_channel(
    channel: Arc<dyn Channel>,
    policy: ChannelRestartPolicy,
    session_defaults: SessionConfigOverrides,
    services: (
        Arc<dyn MessageBus>,
        Arc<dyn SessionStore>,
        Arc<RuntimeUiService>,
    ),
    lifecycle: ChannelReadiness,
    health: Arc<std::sync::RwLock<ChannelHealth>>,
) {
    let (bus, sessions, ui) = services;
    let mut ready_once = false;
    let mut failures = 0_u32;
    loop {
        set_channel_status(
            &health,
            if ready_once {
                ChannelStatus::Restarting
            } else {
                ChannelStatus::Starting
            },
            failures,
        );
        let attempt = lifecycle.next_attempt();
        let ctx = ChannelContext::with_runtime_services_and_defaults(
            bus.clone(),
            sessions.clone(),
            ui.clone(),
            Some(attempt.clone()),
            session_defaults.clone(),
        );
        let run = channel.clone().run(ctx);
        tokio::pin!(run);
        let run_completed = tokio::select! {
            biased;
            () = attempt.wait() => false,
            () = &mut run => true,
        };
        let became_ready = !run_completed || attempt.is_ready();
        if became_ready {
            if !ready_once {
                ready_once = true;
                lifecycle.mark_ready();
            }
            set_channel_status(&health, ChannelStatus::Ready, failures);
            if !run_completed {
                run.await;
            }
        } else if !ready_once {
            set_channel_status(&health, ChannelStatus::Failed, failures);
            return;
        }
        if lifecycle.is_shutdown_requested() {
            set_channel_status(&health, ChannelStatus::Stopped, failures);
            return;
        }
        failures = failures.saturating_add(1);
        if failures > policy.max_attempts {
            set_channel_status(&health, ChannelStatus::Failed, failures);
            return;
        }
        set_channel_status(&health, ChannelStatus::Restarting, failures);
        let exponent = failures.saturating_sub(1).min(31);
        let multiplier = 1_u32 << exponent;
        let delay = policy
            .initial_backoff
            .saturating_mul(multiplier)
            .min(policy.max_backoff);
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = lifecycle.shutdown_requested() => {
                set_channel_status(&health, ChannelStatus::Stopped, failures);
                return;
            }
        }
    }
}

fn set_channel_status(
    health: &std::sync::RwLock<ChannelHealth>,
    status: ChannelStatus,
    restart_count: u32,
) {
    if let Ok(mut health) = health.write() {
        health.status = status;
        health.restart_count = restart_count;
    }
}

// ---------------------------------------------------------------------------
// RuntimeError
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SessionBindingError {
    #[error("session {0} must have exactly one Agent")]
    InvalidMembership(SessionId),
    #[error("session {0} has unresolved registry revision pins")]
    UnresolvedPins(SessionId),
    #[error("session {session_id} belongs to Agent {expected}, not {actual}")]
    AgentMismatch {
        session_id: SessionId,
        expected: AgentId,
        actual: AgentId,
    },
    #[error("Agent {0} has no active immutable revision")]
    MissingActiveAgent(AgentId),
    #[error("Agent {agent_id}@{revision} has no immutable revision")]
    MissingAgentRevision { agent_id: AgentId, revision: u64 },
    #[error("active Agent {agent_id} revision is {expected}, not composed revision {actual}")]
    ActiveAgentMismatch {
        agent_id: AgentId,
        expected: u64,
        actual: u64,
    },
    #[error("Agent {agent_id}@{revision} has no immutable registry snapshot")]
    MissingSnapshot { agent_id: AgentId, revision: u64 },
    #[error("session Provider is {actual}, not snapshot Provider {expected}")]
    ProviderMismatch { expected: String, actual: String },
    #[error("snapshot does not contain Provider {provider_id}")]
    MissingProvider { provider_id: String },
    #[error("snapshot does not contain Model {provider_id}/{model_id}")]
    MissingModel {
        provider_id: String,
        model_id: String,
    },
    #[error("session Provider revision is {actual}, not snapshot revision {expected}")]
    ProviderRevisionMismatch { expected: u64, actual: u64 },
    #[error("session Model revision is {actual}, not snapshot revision {expected}")]
    ModelRevisionMismatch { expected: u64, actual: u64 },
    #[error("Agent registry unavailable while closing session pins")]
    Registry,
    #[error("invalid immutable Agent snapshot")]
    Snapshot,
    #[error(transparent)]
    InvalidPins(#[from] SessionRevisionPinError),
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("engine error: {0}")]
    Engine(String),
    #[error("store error: {0}")]
    Store(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("composition error: {0}")]
    Composition(String),
    #[error("evidence error: {0}")]
    Evidence(String),
    #[error("channel error: {0}")]
    Channel(String),
    #[error(transparent)]
    SessionBinding(#[from] SessionBindingError),
    #[error("{operation} at {path}: {message}")]
    Io {
        operation: &'static str,
        path: String,
        message: String,
    },
}

fn with_resolved_paths(mut config: ServerConfig) -> Result<ServerConfig, RuntimeError> {
    let data_dir = config.server.data_dir.clone().unwrap_or_else(|| {
        std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| std::path::PathBuf::from(home).join(".local/share"))
            })
            .unwrap_or_else(|| std::path::PathBuf::from(".local/share"))
            .join("sylvander")
    });
    std::fs::create_dir_all(&data_dir).map_err(|error| RuntimeError::Io {
        operation: "create data directory",
        path: data_dir.display().to_string(),
        message: error.to_string(),
    })?;
    config.server.data_dir = Some(data_dir.clone());
    config.server.session_db = Some(resolve_durable_database_path(
        "server.session_db",
        config.server.session_db.take(),
        &data_dir,
        "sessions.db",
    )?);
    config.server.memory_db = Some(resolve_durable_database_path(
        "server.memory_db",
        config.server.memory_db.take(),
        &data_dir,
        "memory.db",
    )?);
    config.server.user_profile_db = Some(resolve_durable_database_path(
        "server.user_profile_db",
        config.server.user_profile_db.take(),
        &data_dir,
        "user-profiles.db",
    )?);
    if let Some(MemoryIntegrityBackend::File { anchor_path }) =
        config.server.memory_maintenance.integrity.backend.as_ref()
    {
        validate_external_memory_anchor(&data_dir, anchor_path)?;
    }
    config
        .server
        .workspace_journal
        .get_or_insert_with(|| data_dir.join("workspace-journal"));
    config.server.evidence.path = Some(resolve_durable_database_path(
        "server.evidence.path",
        config.server.evidence.path.take(),
        &data_dir,
        "evidence.db",
    )?);
    if config.server.identity.digest_key.is_some() {
        config
            .server
            .identity
            .database
            .get_or_insert_with(|| data_dir.join("identity.db"));
    }
    Ok(config)
}

fn resolve_durable_database_path(
    field: &str,
    configured: Option<std::path::PathBuf>,
    data_dir: &std::path::Path,
    default_name: &str,
) -> Result<std::path::PathBuf, RuntimeError> {
    let path = match configured {
        Some(path) if path.is_relative() => data_dir.join(path),
        Some(path) => path,
        None => data_dir.join(default_name),
    };
    let mut errors = Vec::new();
    crate::config::validate_durable_database_path(field, &path, true, &mut errors);
    if let Some(message) = errors.into_iter().next() {
        return Err(RuntimeError::Config(message));
    }
    Ok(path)
}

async fn open_identity_binding_service(
    config: &ServerConfig,
) -> Result<Option<Arc<IdentityBindingService>>, RuntimeError> {
    let Some(key_reference) = &config.server.identity.digest_key else {
        return Ok(None);
    };
    let secret = SystemSecretResolver.resolve(key_reference).map_err(|_| {
        RuntimeError::Config("identity principal digest key resolution failed".into())
    })?;
    let digest_key = PrincipalDigestKey::new(secret.as_bytes()).map_err(|_| {
        RuntimeError::Config("identity principal digest key configuration failed".into())
    })?;
    let path = config
        .server
        .identity
        .database
        .as_ref()
        .ok_or_else(|| RuntimeError::Config("identity database path is required".into()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| RuntimeError::Io {
            operation: "create identity database directory",
            path: parent.display().to_string(),
            message: error.to_string(),
        })?;
    }
    let store = PrincipalBindingStore::open(path, digest_key)
        .await
        .map_err(|_| RuntimeError::Store("open principal binding store failed".into()))?;
    let mut registered = BTreeSet::new();
    for issuer in &config.server.identity.trusted_issuers {
        if registered.insert(issuer.user_id.clone()) {
            match store.register_user(UserId::new(&issuer.user_id)).await {
                Ok(()) | Err(PrincipalBindingError::UserAlreadyExists(_)) => {}
                Err(_) => {
                    return Err(RuntimeError::Store(
                        "register stable identity user failed".into(),
                    ));
                }
            }
        }
    }
    let issuers = config.server.identity.trusted_issuers.iter().map(|issuer| {
        TrustedIdentityIssuer::new(
            issuer.transport.clone(),
            issuer.channel_instance_id.clone(),
            issuer.principal_id.clone(),
            UserId::new(&issuer.user_id),
        )
    });
    let service = IdentityBindingService::new(
        store,
        issuers,
        std::time::Duration::from_secs(u64::from(config.server.identity.challenge_ttl_seconds)),
    )
    .map_err(|_| RuntimeError::Config("identity binding service configuration failed".into()))?;
    Ok(Some(Arc::new(service)))
}

fn identity_boundary_error(
    operation: sylvander_protocol::IdentityBindingOperation,
    code: IdentityBindingErrorCode,
    message: &str,
    retry_after_ms: Option<u64>,
) -> IdentityBindingResponse {
    IdentityBindingResponse::Error {
        version: sylvander_protocol::IDENTITY_BINDING_PROTOCOL_VERSION,
        error: IdentityBindingError {
            code,
            operation,
            message: message.into(),
            retry_after_ms,
        },
    }
}

fn validate_external_memory_anchor(
    data_dir: &std::path::Path,
    anchor: &std::path::Path,
) -> Result<(), RuntimeError> {
    let data_dir = std::fs::canonicalize(data_dir).map_err(|_| {
        RuntimeError::Config("memory integrity anchor boundary validation failed".into())
    })?;
    let parent = anchor.parent().ok_or_else(|| {
        RuntimeError::Config("memory integrity anchor boundary validation failed".into())
    })?;
    let parent = std::fs::canonicalize(parent).map_err(|_| {
        RuntimeError::Config("memory integrity anchor boundary validation failed".into())
    })?;
    if parent.starts_with(&data_dir) {
        return Err(RuntimeError::Config(
            "memory integrity anchor must be outside the runtime data directory".into(),
        ));
    }
    if anchor.exists() {
        let metadata = std::fs::metadata(anchor).map_err(|_| {
            RuntimeError::Config("memory integrity anchor boundary validation failed".into())
        })?;
        let resolved = std::fs::canonicalize(anchor).map_err(|_| {
            RuntimeError::Config("memory integrity anchor boundary validation failed".into())
        })?;
        if !metadata.is_file() || resolved.starts_with(&data_dir) {
            return Err(RuntimeError::Config(
                "memory integrity anchor boundary validation failed".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn configure_test_memory_integrity(
    config: &mut ServerConfig,
    directory: &std::path::Path,
    _secret: &std::path::Path,
) {
    let data_dir = directory.join("runtime-data");
    let anchor_dir = directory.join("integrity-anchor");
    std::fs::create_dir_all(&anchor_dir).unwrap();
    config.server.data_dir = Some(data_dir);
    config.server.memory_maintenance.integrity.backend = Some(MemoryIntegrityBackend::File {
        anchor_path: anchor_dir.join("anchor.json"),
    });
    let integrity_key = directory.join("memory-integrity.key");
    std::fs::write(&integrity_key, "0123456789abcdef0123456789abcdef").unwrap();
    config.server.memory_maintenance.integrity.key = Some(crate::config::SecretRef::File {
        path: integrity_key,
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../tests/unit/runtime.rs"]
mod tests;
