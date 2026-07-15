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
//! │  sylvander-runtime│  boot / shutdown / create_ephemeral_session
//! ├──────────────────┤
//! │  sylvander-agent  │  AgentRunEngine / AgentRun / AgentLoop
//! └──────────────────┘
//! ```

mod agent_admin;
pub mod agent_registry;
#[allow(dead_code)] // immutable runtime bindings consumed by registry composition
mod agent_registry_snapshot;
#[cfg(test)]
mod agent_registry_snapshot_tests;
mod boundary;
pub mod composition;
pub mod config;
#[allow(dead_code)] // internal API consumed by credential administration batches
mod credential_registry;
#[cfg(test)]
mod credential_registry_tests;
pub mod evidence;
#[allow(dead_code)] // internal API consumed by model routing/admin batches
mod model_registry;
#[cfg(test)]
mod model_registry_tests;
#[allow(dead_code)] // internal API consumed by provider routing/admin batches
mod provider_registry;
#[cfg(test)]
mod provider_registry_tests;
#[allow(dead_code)] // production handler wiring follows the audited transport seam
mod registry_admin;
#[allow(dead_code)] // pure bootstrap plan; executor wiring follows registry snapshots
mod registry_bootstrap;
#[cfg(test)]
mod registry_bootstrap_tests;
#[allow(dead_code)] // composed by the registry-backed Runtime revision provider
mod registry_composition;
#[cfg(test)]
mod registry_composition_tests;
#[allow(dead_code)] // consumed by the staged registry mutation batches
mod registry_domain;
#[cfg(test)]
mod registry_domain_tests;
#[allow(dead_code)] // wired by registry-backed composition after snapshot resolution
mod request_scoped_provider;

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use sylvander_agent::bus::{InProcessMessageBus, MessageBus};
use sylvander_agent::engine::{AgentRunEngine, RevisionedAgentRunProvider};
use sylvander_agent::session::SessionMetadata;
use sylvander_agent::session_store::{
    SessionLifetime, SessionStore, SqliteSessionStore, StoredSession,
};
use sylvander_agent::spec::{AgentId, AgentSpec, SessionId};
use sylvander_channel::{Channel, ChannelContext, ChannelReadiness};
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_protocol::{
    AgentAdminError, AgentAdminErrorCode, AgentAdminRequest, AgentAdminResponse, AgentAdminResult,
    AgentDescriptor, ModelDescriptor, ModelLifecycle, ReasoningEffort, RegistryAdminError,
    RegistryAdminErrorCode, RegistryAdminRequest, RegistryAdminResponse, RunFeedback,
    SessionConfigOverrides, SessionConfigState, SessionConfigUpdateRequest, SessionCreateRequest,
    SessionEffectiveConfig, SessionRevisionPinError,
};

use crate::agent_admin::{
    AgentAdminDispatch, AgentAdminService, is_agent_administrator, map_registry_error,
    redact_revision,
};
use crate::agent_registry_snapshot::AgentSnapshotSelection;
use crate::composition::{
    ConfiguredAgent, build_agent, build_registry_agent_with_resolver, resolve_session_config,
};
use crate::config::{ServerConfig, SystemSecretResolver};
use crate::credential_registry::CredentialSecretResolver;
use crate::evidence::{AdministrationAudit, AuthorizationDenial, EvidenceRecorder, EvidenceStore};
use crate::registry_admin::{CredentialRegistryMutationService, RegistryAdminService};
use agent_registry::AgentRegistry;
use boundary::BoundaryGuard;

// ---------------------------------------------------------------------------
// SystemConfig
// ---------------------------------------------------------------------------

/// System bootstrap configuration.
///
/// Constructed in code for now; TOML file loading is a future concern.
#[derive(Debug, Clone)]
pub struct SystemConfig {
    /// Human-readable system name.
    pub name: String,
    /// Agents to spawn at boot.
    pub agents: Vec<AgentSpec>,
    /// Pre-defined persistent sessions to load/create at boot.
    pub sessions: Vec<StoredSession>,
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// The system runtime — top-level orchestrator.
pub struct Runtime {
    /// The agent lifecycle engine.
    pub engine: Arc<AgentRunEngine>,
    /// Session persistence backend.
    pub session_store: Arc<dyn SessionStore>,
    /// Ephemeral sessions (tracked in memory, not persisted).
    ephemeral: Arc<RwLock<HashMap<SessionId, StoredSession>>>,
    /// Shared message bus.
    bus: Arc<dyn MessageBus>,
    /// Fully configured runs retained for protocol control operations.
    configured_agents: HashMap<AgentId, ConfiguredAgent>,
    revision_provider: Option<Arc<RuntimeRevisionProvider>>,
    ui_service: Arc<RuntimeUiService>,
    evidence: Option<EvidenceRecorder>,
    channels: tokio::sync::Mutex<Vec<ChannelTask>>,
    channel_exit_tx: tokio::sync::mpsc::UnboundedSender<String>,
    channel_exits: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>,
}

struct ChannelTask {
    name: String,
    task: JoinHandle<()>,
    lifecycle: ChannelReadiness,
}

struct RuntimeUiService {
    engine: Arc<AgentRunEngine>,
    sessions: Arc<dyn SessionStore>,
    agents: HashMap<AgentId, ConfiguredAgent>,
    agent_registry: Option<AgentRegistry>,
    revision_provider: Option<Arc<RuntimeRevisionProvider>>,
    credential_resolver: Option<Arc<dyn CredentialSecretResolver>>,
    evidence: Option<EvidenceStore>,
    boundary: BoundaryGuard,
}

struct RuntimeRevisionProvider {
    config: ServerConfig,
    registry: AgentRegistry,
    bus: Arc<dyn MessageBus>,
    sessions: Arc<dyn SessionStore>,
    ephemeral: Arc<RwLock<HashMap<SessionId, StoredSession>>>,
    credential_resolver: Arc<dyn CredentialSecretResolver>,
    configured: RwLock<HashMap<(AgentId, u64), ConfiguredAgent>>,
}

impl RuntimeRevisionProvider {
    fn preflight_definition(
        &self,
        definition: &crate::config::AgentDefinitionConfig,
    ) -> Result<(), RuntimeError> {
        build_agent(
            &self.config,
            definition,
            self.bus.clone(),
            self.sessions.clone(),
            &SystemSecretResolver,
        )
        .map(|_| ())
        .map_err(|error| RuntimeError::Composition(error.to_string()))
    }

    async fn compose_revision(
        &self,
        agent_id: &AgentId,
        revision: u64,
    ) -> Result<ConfiguredAgent, RuntimeError> {
        let snapshot = self
            .registry
            .resolve_registry_composition(agent_id, revision)
            .await
            .map_err(|error| RuntimeError::Composition(error.to_string()))?;
        build_registry_agent_with_resolver(
            &self.config,
            snapshot,
            self.registry.clone(),
            self.bus.clone(),
            self.sessions.clone(),
            self.credential_resolver.clone(),
        )
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
        if let Some(session) = self
            .sessions
            .get(session_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
        {
            return self.bound_stored_revision(agent_id, &session).await;
        }
        let ephemeral = self
            .ephemeral
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| RuntimeError::Config(format!("session {session_id} is not bound")))?;
        self.bound_stored_revision(agent_id, &ephemeral).await
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
    config: &ServerConfig,
    definition: &crate::config::AgentDefinitionConfig,
) -> Result<AgentSnapshotSelection, RuntimeError> {
    let provider_id = &definition.spec.model.provider;
    let provider = config
        .model_providers
        .iter()
        .find(|candidate| &candidate.id == provider_id)
        .ok_or_else(|| RuntimeError::Config(format!("unknown Provider `{provider_id}`")))?;
    let allowed_model_ids = provider
        .models
        .iter()
        .map(|model| model.id.clone())
        .collect::<BTreeSet<_>>();
    Ok(AgentSnapshotSelection {
        agent_id: definition.spec.id.to_string(),
        agent_revision: definition.revision,
        provider_id: provider_id.clone(),
        allowed_model_ids,
        default_model_id: definition.spec.model.model_name.clone(),
    })
}

struct SessionPinClosure {
    effective: SessionEffectiveConfig,
    changed: bool,
}

async fn close_session_revision_pins(
    registry: &AgentRegistry,
    session: &StoredSession,
    active_agent: &ConfiguredAgent,
) -> Result<SessionPinClosure, SessionBindingError> {
    let [member] = session.agents.as_slice() else {
        return Err(SessionBindingError::InvalidMembership(session.id.clone()));
    };
    let (mut effective, mut changed) = if let Some(effective) = &session.effective_config {
        if member != &effective.agent_id {
            return Err(SessionBindingError::AgentMismatch {
                session_id: session.id.clone(),
                expected: member.clone(),
                actual: effective.agent_id.clone(),
            });
        }
        (effective.clone(), false)
    } else {
        if member != &active_agent.spec.id {
            return Err(SessionBindingError::AgentMismatch {
                session_id: session.id.clone(),
                expected: member.clone(),
                actual: active_agent.spec.id.clone(),
            });
        }
        let active = registry
            .load_active(member)
            .await
            .map_err(|_| SessionBindingError::Registry)?
            .ok_or_else(|| SessionBindingError::MissingActiveAgent(member.clone()))?;
        if active.definition.revision != active_agent.definition.revision {
            return Err(SessionBindingError::ActiveAgentMismatch {
                agent_id: member.clone(),
                expected: active.definition.revision,
                actual: active_agent.definition.revision,
            });
        }
        let effective = resolve_session_config(
            active_agent,
            &session.config_overrides,
            None,
            Some(&session.metadata.workspace),
        )
        .map_err(|_| SessionBindingError::Resolution)?;
        (effective, true)
    };
    let snapshot = registry
        .load_agent_snapshot(&effective.agent_id.0, effective.agent_revision)
        .await
        .map_err(|_| SessionBindingError::Snapshot)?
        .ok_or_else(|| SessionBindingError::MissingSnapshot {
            agent_id: effective.agent_id.clone(),
            revision: effective.agent_revision,
        })?;
    snapshot
        .validate()
        .map_err(|_| SessionBindingError::Snapshot)?;
    if snapshot.provider_id != effective.provider_id {
        return Err(SessionBindingError::ProviderMismatch {
            expected: snapshot.provider_id,
            actual: effective.provider_id,
        });
    }
    let model = snapshot
        .models
        .iter()
        .find(|model| {
            model.provider_id == effective.provider_id && model.model_id == effective.model_id
        })
        .ok_or_else(|| SessionBindingError::MissingModel {
            provider_id: effective.provider_id.clone(),
            model_id: effective.model_id.clone(),
        })?;
    match effective.provider_revision {
        Some(actual) if actual != snapshot.provider_revision => {
            return Err(SessionBindingError::ProviderRevisionMismatch {
                expected: snapshot.provider_revision,
                actual,
            });
        }
        None => {
            effective.provider_revision = Some(snapshot.provider_revision);
            changed = true;
        }
        Some(_) => {}
    }
    match effective.model_revision {
        Some(actual) if actual != model.revision => {
            return Err(SessionBindingError::ModelRevisionMismatch {
                expected: model.revision,
                actual,
            });
        }
        None => {
            effective.model_revision = Some(model.revision);
            changed = true;
        }
        Some(_) => {}
    }
    effective
        .require_revision_pins()
        .map_err(SessionBindingError::InvalidPins)?;
    Ok(SessionPinClosure { effective, changed })
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
            agents.push(AgentDescriptor {
                id: agent.spec.id.clone(),
                revision: agent.definition.revision,
                name: agent.spec.name.clone(),
                provider_id: agent.spec.model.provider.clone(),
                default_model_id: agent.spec.model.model_name.clone(),
                models: agent
                    .models
                    .iter()
                    .map(|model| ModelDescriptor {
                        id: model.id.clone(),
                        provider: agent.spec.model.provider.clone(),
                        capabilities: model.capabilities.bits(),
                        reasoning_efforts: if model.capabilities.contains(
                            sylvander_llm_anthropic::api::model::ModelCapabilities::EXTENDED_THINKING,
                        ) {
                            vec![
                                ReasoningEffort::Off,
                                ReasoningEffort::Low,
                                ReasoningEffort::Medium,
                                ReasoningEffort::High,
                            ]
                        } else {
                            vec![ReasoningEffort::Off]
                        },
                        lifecycle: ModelLifecycle::Active,
                        pricing: None,
                    })
                    .collect(),
                default_prompt_profile: agent.definition.default_prompt_profile.clone(),
                agent_workspace: agent.definition.agent_workspace.as_ref().map(|workspace| {
                    sylvander_protocol::SessionWorkspaceBinding {
                        execution_target: workspace.execution_target.clone(),
                        path: workspace.path.clone().into(),
                        read_only: workspace.read_only,
                    }
                }),
            });
        }
        agents.sort_by(|left, right| left.id.0.cmp(&right.id.0));
        Ok(agents)
    }

    async fn create_session(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: SessionCreateRequest,
    ) -> Result<SessionConfigState, sylvander_protocol::BoundaryError> {
        let principal = require_principal(boundary, "create_session")?;
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
        let effective = resolve_session_config(&agent, &request.overrides, None, None)
            .map_err(|error| boundary_failure(boundary, "create_session", error.to_string()))?;
        let workspace = effective
            .user_workspace
            .as_ref()
            .or(effective.agent_workspace.as_ref())
            .map_or_else(
                || std::path::PathBuf::from("."),
                |binding| binding.path.clone(),
            );
        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let metadata = SessionMetadata {
            workspace,
            name: label.clone(),
            user_id: principal.id.0.clone(),
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
        session.external_meta.insert(
            "channel_id".into(),
            serde_json::Value::String(boundary.channel_instance_id.clone()),
        );
        self.sessions
            .save(&session)
            .await
            .map_err(|error| boundary_failure(boundary, "create_session", error.to_string()))?;
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
            let _ = self.sessions.delete(&session_id).await;
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
        let effective = resolve_session_config(
            &agent,
            &request.overrides,
            None,
            Some(&session.metadata.workspace),
        )
        .map_err(|error| boundary_failure(boundary, "update_session_config", error.to_string()))?;
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
        require_principal(boundary, "submit_feedback")?;
        if feedback.note.as_ref().is_some_and(|note| note.len() > 4096) {
            return Err(boundary_failure(
                boundary,
                "submit_feedback",
                "feedback note exceeds 4096 bytes",
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
        let store = self.evidence.as_ref().ok_or_else(|| {
            boundary_failure(
                boundary,
                "submit_feedback",
                "runtime evidence capture is disabled",
            )
        })?;
        let session_id = store
            .feedback_session(feedback.run_id.clone(), feedback.turn_id.clone())
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
        store
            .record_feedback(feedback, sylvander_agent::session::now_secs())
            .await
            .map_err(|error| boundary_failure(boundary, "submit_feedback", error.to_string()))
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
            } => match (
                provider.preflight_definition(&definition),
                active_snapshot_selection(&provider.config, &definition),
            ) {
                (Ok(()), Ok(selection)) => match registry
                    .update(&provider.config, expected_active_revision, *definition)
                    .await
                {
                    Ok(stored) => {
                        if registry.stage_agent_snapshot(selection).await.is_err()
                            || provider
                                .configured_revision(
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
                    Err(error) => AgentAdminResponse::Error {
                        error: map_registry_error(error),
                    },
                },
                _ => agent_admin_error(
                    AgentAdminErrorCode::InvalidDefinition,
                    "Agent revision could not be composed",
                ),
            },
            AgentAdminDispatch::Activate {
                agent_id,
                revision,
                expected_active_revision,
            } => match provider.configured_revision(&agent_id, revision).await {
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
                .configured_revision(&agent_id, target_revision)
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
                CredentialRegistryMutationService::new(registry, resolver)
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
                .feedback_session(feedback.run_id.clone(), feedback.turn_id.clone())
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
        let principal = require_principal(boundary, operation)?;
        let session = self
            .sessions
            .get(session_id)
            .await
            .map_err(|error| boundary_failure(boundary, operation, error.to_string()))?
            .ok_or_else(|| sylvander_protocol::BoundaryError::forbidden(boundary, operation))?;
        let owns_principal = session.metadata.user_id == principal.id.0;
        let owns_channel = session
            .external_meta
            .get("channel_id")
            .and_then(|value| value.as_str())
            == Some(boundary.channel_instance_id.as_str());
        if (!owns_principal || !owns_channel) && !privileged_principal(boundary) {
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
        Message::AgentAdmin { .. } => "agent_admin",
        Message::RegistryAdmin { .. } => "registry_admin",
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
        | Message::RollbackWorkspace { session_id, .. } => Some(session_id),
        Message::UpdateSessionConfig { request } => Some(&request.session_id.0),
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
    /// Bootstrap the system.
    ///
    /// # Flow
    ///
    /// 1. Create an in-process message bus
    /// 2. Create the engine
    /// 3. Create an in-memory session store
    /// 4. Load persistent sessions → re-create in engine
    /// 5. Spawn each agent from config
    /// 6. Create sessions defined in config
    pub async fn boot(
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

        // Spawn agents
        for spec in &config.agents {
            engine
                .spawn(spec.clone(), default_client.clone())
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
            sessions: session_store.clone(),
            agents: configured_agents.clone(),
            agent_registry: None,
            revision_provider: None,
            credential_resolver: None,
            evidence: None,
            boundary: BoundaryGuard::new(crate::config::BoundarySettings::default()),
        });
        Ok(Self {
            engine,
            session_store,
            ephemeral: Arc::new(RwLock::new(HashMap::new())),
            bus,
            configured_agents,
            revision_provider: None,
            ui_service,
            evidence: None,
            channels: tokio::sync::Mutex::new(Vec::new()),
            channel_exit_tx,
            channel_exits: tokio::sync::Mutex::new(channel_exits),
        })
    }

    /// Bootstrap the production runtime from validated server configuration.
    pub async fn boot_config(config: ServerConfig) -> Result<Self, RuntimeError> {
        config
            .validate()
            .map_err(|error| RuntimeError::Config(error.to_string()))?;
        let mut config = with_resolved_paths(config)?;
        let session_db = config
            .server
            .session_db
            .as_ref()
            .expect("resolved session database");
        if let Some(parent) = session_db.parent() {
            std::fs::create_dir_all(parent).map_err(|error| RuntimeError::Io {
                operation: "create session database directory",
                path: parent.display().to_string(),
                message: error.to_string(),
            })?;
        }

        let agent_registry = AgentRegistry::open(session_db)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?;
        let credential_resolver: Arc<dyn CredentialSecretResolver> = Arc::new(SystemSecretResolver);
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
        config.agents = active_definitions;
        config.validate().map_err(|error| {
            RuntimeError::Config(format!("active Agent registry is incompatible: {error}"))
        })?;
        for definition in &config.agents {
            let existing = agent_registry
                .load_agent_snapshot(&definition.spec.id.0, definition.revision)
                .await
                .map_err(|error| RuntimeError::Composition(error.to_string()))?;
            if existing.is_none() {
                let selection = active_snapshot_selection(&config, definition)?;
                agent_registry
                    .stage_agent_snapshot(selection)
                    .await
                    .map_err(|error| RuntimeError::Composition(error.to_string()))?;
            }
        }

        let session_store: Arc<dyn SessionStore> = Arc::new(
            SqliteSessionStore::open(session_db)
                .await
                .map_err(|error| RuntimeError::Store(error.to_string()))?,
        );
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
        // Security denials are always durable even when optional run-content
        // evidence collection is disabled by the operator.
        let security_audit = EvidenceStore::open(evidence_path)
            .await
            .map_err(|error| RuntimeError::Evidence(error.to_string()))?;
        let evidence = if config.server.evidence.enabled {
            Some(
                EvidenceRecorder::start(
                    bus.clone(),
                    security_audit.clone(),
                    config.server.name.clone(),
                    config.server.evidence.content,
                    config.server.evidence.retention_days,
                )
                .await
                .map_err(|error| RuntimeError::Evidence(error.to_string()))?,
            )
        } else {
            None
        };
        let mut agents = Vec::with_capacity(config.agents.len());
        for definition in &config.agents {
            let snapshot = agent_registry
                .resolve_registry_composition(&definition.spec.id, definition.revision)
                .await
                .map_err(|error| RuntimeError::Composition(error.to_string()))?;
            agents.push(
                build_registry_agent_with_resolver(
                    &config,
                    snapshot,
                    agent_registry.clone(),
                    bus.clone(),
                    session_store.clone(),
                    credential_resolver.clone(),
                )
                .map_err(|error| RuntimeError::Composition(error.to_string()))?,
            );
        }
        let ephemeral = Arc::new(RwLock::new(HashMap::new()));
        let mut configured_agents = HashMap::new();
        for agent in agents {
            configured_agents.insert(agent.spec.id.clone(), agent);
        }
        let revision_provider = Arc::new(RuntimeRevisionProvider {
            config: config.clone(),
            registry: agent_registry.clone(),
            bus: bus.clone(),
            sessions: session_store.clone(),
            ephemeral: ephemeral.clone(),
            credential_resolver: credential_resolver.clone(),
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

        for mut session in session_store
            .list_persistent()
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
        {
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

        info!(
            name = %config.server.name,
            agents = configured_agents.len(),
            session_db = %session_db.display(),
            "configured runtime booted"
        );
        let ui_service = Arc::new(RuntimeUiService {
            engine: engine.clone(),
            sessions: session_store.clone(),
            agents: configured_agents.clone(),
            agent_registry: Some(agent_registry.clone()),
            revision_provider: Some(revision_provider.clone()),
            credential_resolver: Some(credential_resolver),
            evidence: Some(security_audit),
            boundary: BoundaryGuard::new(config.server.boundary.clone()),
        });
        let (channel_exit_tx, channel_exits) = tokio::sync::mpsc::unbounded_channel();
        Ok(Self {
            engine,
            session_store,
            ephemeral,
            bus,
            configured_agents,
            revision_provider: Some(revision_provider),
            ui_service,
            evidence,
            channels: tokio::sync::Mutex::new(Vec::new()),
            channel_exit_tx,
            channel_exits: tokio::sync::Mutex::new(channel_exits),
        })
    }

    /// Return protocol metadata and control for one configured Agent.
    #[must_use]
    pub fn configured_agent(&self, id: &AgentId) -> Option<&ConfiguredAgent> {
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
        let effective =
            resolve_session_config(&agent, &overrides, None, Some(&session.metadata.workspace))
                .map_err(|error| {
                    RuntimeError::Config(format!(
                        "resolve configuration for session {session_id}: {error}"
                    ))
                })?;
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
        channels: Vec<Arc<dyn Channel>>,
    ) -> Result<(), RuntimeError> {
        let mut tasks = self.channels.lock().await;
        if !tasks.is_empty() {
            return Err(RuntimeError::Channel(
                "channels have already been started".into(),
            ));
        }
        for ch in channels {
            let readiness = ChannelReadiness::new();
            let ctx = ChannelContext {
                bus: self.bus.clone(),
                sessions: self.session_store.clone(),
                ui: Some(self.ui_service.clone()),
                readiness: Some(readiness.clone()),
            };
            let name = ch.name().to_string();
            let task_name = name.clone();
            let exit_tx = self.channel_exit_tx.clone();
            let mut task = tokio::spawn(async move {
                let _exit_signal = ChannelExitSignal {
                    name: task_name,
                    sender: exit_tx,
                };
                ch.run(ctx).await;
            });
            let startup = tokio::select! {
                result = &mut task => {
                    Err(RuntimeError::Channel(match result {
                        Ok(()) => format!("channel {name} exited before becoming ready"),
                        Err(error) => format!("channel {name} failed during startup: {error}"),
                    }))
                }
                result = tokio::time::timeout(
                    tokio::time::Duration::from_secs(5),
                    readiness.wait(),
                ) => {
                    if result.is_err() {
                        task.abort();
                        let _ = (&mut task).await;
                        Err(RuntimeError::Channel(format!(
                            "channel {name} did not become ready within 5 seconds"
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
            info!(channel = %name, "channel ready");
            tasks.push(ChannelTask {
                name,
                task,
                lifecycle: readiness,
            });
        }
        Ok(())
    }

    /// Wait until a started channel exits unexpectedly.
    pub async fn wait_for_channel_exit(&self) -> Option<String> {
        self.channel_exits.lock().await.recv().await
    }

    /// Wait until an Agent task exits without a matching shutdown request.
    pub async fn wait_for_agent_exit(&self) -> Option<AgentId> {
        self.engine.wait_for_agent_exit().await
    }

    // -- ephemeral sessions --

    /// Create a temporary session.
    ///
    /// This is the primary entry point for channels creating
    /// per-conversation sessions (new TUI window, new Telegram chat).
    pub async fn create_ephemeral_session(
        &self,
        name: impl Into<String>,
        metadata: SessionMetadata,
        agents: &[AgentId],
        external_meta: HashMap<String, serde_json::Value>,
    ) -> Result<SessionId, RuntimeError> {
        let name = name.into();
        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let session_name = metadata.name.clone();
        if self.revision_provider.is_some() && agents.len() != 1 {
            return Err(RuntimeError::Config(
                "revisioned ephemeral sessions require exactly one Agent".into(),
            ));
        }
        let mut stored = StoredSession::new(
            session_id.clone(),
            session_name,
            SessionLifetime::Ephemeral,
            metadata.clone(),
            agents.to_vec(),
        );
        stored.external_meta = external_meta;
        if let Some(provider) = &self.revision_provider {
            let agent_id = agents.first().ok_or_else(|| {
                RuntimeError::Config("ephemeral session requires one Agent".into())
            })?;
            let agent = provider.active_agent(agent_id).await?;
            stored.effective_config = Some(
                resolve_session_config(
                    &agent,
                    &stored.config_overrides,
                    None,
                    Some(&stored.metadata.workspace),
                )
                .map_err(|error| RuntimeError::Composition(error.to_string()))?,
            );
        }

        self.ephemeral
            .write()
            .await
            .insert(session_id.clone(), stored);

        if let Err(error) = self
            .engine
            .attach_session(session_id.clone(), name, metadata, agents)
            .await
        {
            self.ephemeral.write().await.remove(&session_id);
            return Err(RuntimeError::Engine(format!("create ephemeral: {error}")));
        }

        Ok(session_id)
    }

    /// Look up external metadata for an ephemeral session.
    pub async fn get_external_meta(
        &self,
        session_id: &SessionId,
    ) -> Option<HashMap<String, serde_json::Value>> {
        self.ephemeral
            .read()
            .await
            .get(session_id)
            .map(|s| s.external_meta.clone())
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
        if let Some(evidence) = &self.evidence
            && let Err(error) = evidence.shutdown().await
        {
            first_error.get_or_insert_with(|| RuntimeError::Evidence(error.to_string()));
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
            warn!(channel = %channel.name, "channel drain timed out; aborting task");
            channel.task.abort();
            channel.task.await
        };
        match result {
            Ok(()) => info!(channel = %channel.name, "channel stopped"),
            Err(error) if error.is_cancelled() => {
                info!(channel = %channel.name, "channel cancelled during shutdown");
            }
            Err(error) => {
                first_error.get_or_insert_with(|| {
                    RuntimeError::Channel(format!("channel {} task failed: {error}", channel.name))
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
    #[error("snapshot does not contain Model {provider_id}/{model_id}")]
    MissingModel {
        provider_id: String,
        model_id: String,
    },
    #[error("session Provider revision is {actual}, not snapshot revision {expected}")]
    ProviderRevisionMismatch { expected: u64, actual: u64 },
    #[error("session Model revision is {actual}, not snapshot revision {expected}")]
    ModelRevisionMismatch { expected: u64, actual: u64 },
    #[error("failed to resolve legacy session configuration")]
    Resolution,
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
    config
        .server
        .session_db
        .get_or_insert_with(|| data_dir.join("sessions.db"));
    config
        .server
        .workspace_journal
        .get_or_insert_with(|| data_dir.join("workspace-journal"));
    config
        .server
        .evidence
        .path
        .get_or_insert_with(|| data_dir.join("evidence.db"));
    Ok(config)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct BlockingChannel {
        started: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    }

    struct ExitingChannel;

    struct ReadyThenExitChannel {
        exit: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl Channel for ExitingChannel {
        fn name(&self) -> &'static str {
            "exiting-test"
        }

        async fn run(self: Arc<Self>, _ctx: ChannelContext) {}
    }

    #[async_trait::async_trait]
    impl Channel for ReadyThenExitChannel {
        fn name(&self) -> &'static str {
            "ready-then-exit-test"
        }

        async fn run(self: Arc<Self>, ctx: ChannelContext) {
            ctx.mark_ready();
            self.exit.notified().await;
        }
    }

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl Channel for BlockingChannel {
        fn name(&self) -> &'static str {
            "blocking-test"
        }

        async fn run(self: Arc<Self>, ctx: ChannelContext) {
            let _drop_signal = DropSignal(self.dropped.clone());
            ctx.mark_ready();
            self.started.notify_one();
            ctx.shutdown_requested().await;
        }
    }

    fn test_spec(id: &str) -> AgentSpec {
        AgentSpec::builder()
            .id(id)
            .name(format!("Agent {id}"))
            .model_name("claude-sonnet-5-20260601")
            .build()
            .expect("spec")
    }

    fn test_client() -> AnthropicClient {
        AnthropicClient::builder()
            .api_key("test-key")
            .build()
            .expect("client")
    }

    fn test_metadata() -> SessionMetadata {
        SessionMetadata {
            workspace: PathBuf::from("/tmp"),
            name: "test".into(),
            user_id: "user-1".into(),
        }
    }

    #[tokio::test]
    async fn boot_spawns_agents() {
        let config = SystemConfig {
            name: "test-runtime".into(),
            agents: vec![test_spec("agent-1"), test_spec("agent-2")],
            sessions: vec![],
        };

        let rt = Runtime::boot(config, test_client()).await.expect("boot");
        assert_eq!(rt.engine.list_agents().await.len(), 2);
        rt.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn shutdown_cancels_owned_channel_tasks_before_returning() {
        let runtime = Runtime::boot(
            SystemConfig {
                name: "test-runtime".into(),
                agents: Vec::new(),
                sessions: Vec::new(),
            },
            test_client(),
        )
        .await
        .unwrap();
        let started = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        runtime
            .start_channels(vec![Arc::new(BlockingChannel {
                started: started.clone(),
                dropped: dropped.clone(),
            })])
            .await
            .unwrap();
        started.notified().await;

        runtime.shutdown().await.unwrap();
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn channel_exit_before_readiness_fails_startup() {
        let runtime = Runtime::boot(
            SystemConfig {
                name: "test-runtime".into(),
                agents: Vec::new(),
                sessions: Vec::new(),
            },
            test_client(),
        )
        .await
        .unwrap();

        let error = runtime
            .start_channels(vec![Arc::new(ExitingChannel)])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("before becoming ready"));
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn startup_failure_drains_channels_that_are_already_ready() {
        let runtime = Runtime::boot(
            SystemConfig {
                name: "test-runtime".into(),
                agents: Vec::new(),
                sessions: Vec::new(),
            },
            test_client(),
        )
        .await
        .unwrap();
        let dropped = Arc::new(AtomicBool::new(false));

        let error = runtime
            .start_channels(vec![
                Arc::new(BlockingChannel {
                    started: Arc::new(Notify::new()),
                    dropped: dropped.clone(),
                }),
                Arc::new(ExitingChannel),
            ])
            .await
            .unwrap_err();

        assert!(error.to_string().contains("before becoming ready"));
        assert!(dropped.load(Ordering::SeqCst));
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn channel_exit_after_readiness_is_reported() {
        let runtime = Runtime::boot(
            SystemConfig {
                name: "test-runtime".into(),
                agents: Vec::new(),
                sessions: Vec::new(),
            },
            test_client(),
        )
        .await
        .unwrap();
        let exit = Arc::new(Notify::new());
        runtime
            .start_channels(vec![Arc::new(ReadyThenExitChannel { exit: exit.clone() })])
            .await
            .unwrap();

        exit.notify_one();
        let channel = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            runtime.wait_for_channel_exit(),
        )
        .await
        .unwrap();
        assert_eq!(channel.as_deref(), Some("ready-then-exit-test"));
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn boot_loads_persistent_sessions() {
        let config = SystemConfig {
            name: "test-runtime".into(),
            agents: vec![test_spec("agent-1")],
            sessions: vec![StoredSession::new(
                SessionId::new("persistent-1"),
                "persistent-chat",
                SessionLifetime::Persistent,
                test_metadata(),
                vec![AgentId::new("agent-1")],
            )],
        };

        let rt = Runtime::boot(config, test_client()).await.expect("boot");
        assert_eq!(rt.engine.list_sessions().await.len(), 1);
        assert!(
            rt.engine
                .get_session(&SessionId::new("persistent-1"))
                .await
                .is_some()
        );
        rt.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn configured_boot_restores_database_session_after_agent_spawn() {
        let model_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_revision_probe",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "configured revision"}],
                "model": "model-a",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 4, "output_tokens": 2}
            })))
            .mount(&model_server)
            .await;
        let directory = tempfile::TempDir::new().unwrap();
        let database = directory.path().join("sessions.db");
        let secret = directory.path().join("provider.key");
        std::fs::write(&secret, "test-secret").unwrap();
        let store = SqliteSessionStore::open(&database).await.unwrap();
        store
            .save(&StoredSession::new(
                SessionId::new("restored-session"),
                "restored",
                SessionLifetime::Persistent,
                test_metadata(),
                vec![AgentId::new("assistant")],
            ))
            .await
            .unwrap();
        drop(store);

        let input = format!(
            r#"
schema_version = 1

[server]
data_dir = "{}"
session_db = "{}"

[[model_providers]]
id = "primary"
base_url = "{}"

[model_providers.api_key]
source = "file"
path = "{}"

[[model_providers.models]]
id = "model-a"
capabilities = ["tool_use"]

[[model_providers.models]]
id = "model-b"
capabilities = ["tool_use"]

[[agents]]
allow_session_prompt = false

[agents.access]
allowed_principals = ["test-user", "telegram:bot-a:42"]

[agents.spec]
id = "assistant"
name = "Sylvander"

[agents.spec.model]
provider = "primary"
model_name = "model-a"
"#,
            directory.path().display(),
            database.display(),
            model_server.uri(),
            secret.display()
        );
        let mut config = ServerConfig::from_toml(&input).unwrap();
        config.agents[0].spec.persona.system_prompt = "revision one prompt".into();
        let restart_config = config.clone();
        let runtime = Runtime::boot_config(config).await.unwrap();

        assert!(
            runtime
                .engine
                .get_session(&SessionId::new("restored-session"))
                .await
                .is_some()
        );
        assert!(
            runtime
                .configured_agent(&AgentId::new("assistant"))
                .is_some()
        );
        let migrated = runtime
            .session_store
            .get(&SessionId::new("restored-session"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(migrated.config_revision, 1);
        let effective = migrated.effective_config.unwrap();
        assert_eq!(effective.agent_id, AgentId::new("assistant"));
        assert_eq!(effective.model_id, "model-a");
        let pins = effective.require_revision_pins().unwrap();
        assert_eq!(pins.provider_revision, 1);
        assert_eq!(pins.model_revision, 1);
        assert_eq!(effective.execution_target, "local");
        assert_eq!(
            effective.provenance.user_workspace.kind,
            sylvander_protocol::SessionConfigSourceKind::LegacyMigration
        );
        let registry = runtime.ui_service.agent_registry.as_ref().unwrap();
        let active_agent = runtime
            .configured_agent(&AgentId::new("assistant"))
            .unwrap();
        let mut legacy = StoredSession::new(
            SessionId::new("legacy-pin-probe"),
            "legacy pin probe",
            SessionLifetime::Persistent,
            test_metadata(),
            vec![AgentId::new("assistant")],
        );
        let mut unpinned = effective.clone();
        unpinned.provider_revision = None;
        unpinned.model_revision = None;
        legacy.effective_config = Some(unpinned);
        let closed = close_session_revision_pins(registry, &legacy, active_agent)
            .await
            .unwrap();
        assert!(closed.changed);
        assert_eq!(closed.effective.require_revision_pins().unwrap(), pins);
        runtime.session_store.save(&legacy).await.unwrap();
        assert!(
            runtime
                .revision_provider
                .as_ref()
                .unwrap()
                .revision_for_session(&AgentId::new("assistant"), &legacy.id)
                .await
                .is_err(),
            "execution routing must not repair unresolved pins on demand"
        );
        runtime.session_store.delete(&legacy.id).await.unwrap();

        legacy.effective_config = Some(effective.clone());
        let already_closed = close_session_revision_pins(registry, &legacy, active_agent)
            .await
            .unwrap();
        assert!(!already_closed.changed);

        let mut mismatched = legacy;
        let mut invalid = effective.clone();
        invalid.model_revision = Some(99);
        mismatched.effective_config = Some(invalid);
        assert!(matches!(
            close_session_revision_pins(registry, &mismatched, active_agent).await,
            Err(SessionBindingError::ModelRevisionMismatch {
                expected: 1,
                actual: 99
            })
        ));
        let (revision, updated) = runtime
            .update_session_config(
                &SessionId::new("restored-session"),
                1,
                SessionConfigOverrides {
                    model_id: Some("model-a".into()),
                    ..SessionConfigOverrides::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(revision, 2);
        assert_eq!(
            updated.provenance.model.kind,
            sylvander_protocol::SessionConfigSourceKind::SessionOverride
        );
        assert!(
            runtime
                .update_session_config(
                    &SessionId::new("restored-session"),
                    1,
                    SessionConfigOverrides::default(),
                )
                .await
                .is_err(),
            "a stale client must not overwrite a newer configuration"
        );
        let owner = sylvander_protocol::BoundaryContext::authenticated(
            sylvander_protocol::AuthenticatedPrincipal::user(
                "test-user",
                sylvander_protocol::AuthenticationMethod::UnixPeer,
            ),
            "tui-local",
            "unix",
            "request-create",
        );
        let created = sylvander_channel::UiService::create_session(
            runtime.ui_service.as_ref(),
            &owner,
            SessionCreateRequest {
                agent_id: AgentId::new("assistant"),
                label: "created through UI service".into(),
                channel_id: Some("tui-local".into()),
                overrides: SessionConfigOverrides {
                    model_id: Some("model-a".into()),
                    ..SessionConfigOverrides::default()
                },
            },
        )
        .await
        .unwrap();
        assert!(created.effective.require_revision_pins().is_ok());
        let stored = runtime
            .session_store
            .get(&created.session_id)
            .await
            .unwrap()
            .expect("created session must be durable");
        assert_eq!(stored.effective_config, Some(created.effective));
        assert_eq!(stored.metadata.user_id, "test-user");
        assert_eq!(stored.external_meta["channel_id"], "tui-local");
        assert!(
            runtime
                .revision_provider
                .as_ref()
                .unwrap()
                .revision_for_session(&AgentId::new("different-agent"), &created.session_id)
                .await
                .is_err(),
            "a session revision binding must never be reused for another Agent"
        );
        let peer = sylvander_channel::UiService::create_session(
            runtime.ui_service.as_ref(),
            &owner,
            SessionCreateRequest {
                agent_id: AgentId::new("assistant"),
                label: "unmodified peer session".into(),
                channel_id: Some("tui-local".into()),
                overrides: SessionConfigOverrides::default(),
            },
        )
        .await
        .unwrap();
        let restricted = sylvander_protocol::PermissionProfile {
            file_access: sylvander_protocol::FileAccess::ReadOnly,
            network_access: sylvander_protocol::NetworkAccess::Denied,
            approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
        };
        let selected = sylvander_channel::UiService::update_session_config(
            runtime.ui_service.as_ref(),
            &owner,
            SessionConfigUpdateRequest {
                session_id: created.session_id.clone(),
                expected_revision: created.revision,
                overrides: SessionConfigOverrides {
                    model_id: Some("model-a".into()),
                    permissions: Some(restricted.clone()),
                    ..SessionConfigOverrides::default()
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(selected.effective.permissions, restricted);
        let peer_after = sylvander_channel::UiService::session_config(
            runtime.ui_service.as_ref(),
            &owner,
            &peer.session_id,
        )
        .await
        .unwrap();
        assert_eq!(
            peer_after, peer,
            "one session override must not leak to another"
        );
        let missing_session = sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &owner,
            &sylvander_protocol::UiClientMessage::SelectModel {
                session_id: None,
                model: "model-a".into(),
                reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
            },
        )
        .await
        .expect_err("legacy selection without session identity must fail closed");
        assert_eq!(
            missing_session.code,
            sylvander_protocol::BoundaryErrorCode::Forbidden
        );
        let other_terminal = sylvander_protocol::BoundaryContext::authenticated(
            sylvander_protocol::AuthenticatedPrincipal::user(
                "test-user",
                sylvander_protocol::AuthenticationMethod::UnixPeer,
            ),
            "other-terminal",
            "unix",
            "request-cross-instance",
        );
        let denial = sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &other_terminal,
            &sylvander_protocol::UiClientMessage::GetSessionConfig {
                session_id: created.session_id.0.clone(),
            },
        )
        .await
        .expect_err("the same principal from another channel instance must be denied");
        assert_eq!(
            denial.code,
            sylvander_protocol::BoundaryErrorCode::Forbidden
        );
        let platform_boundary = sylvander_protocol::BoundaryContext::authenticated(
            sylvander_protocol::AuthenticatedPrincipal::user(
                "telegram:bot-a:42",
                sylvander_protocol::AuthenticationMethod::PlatformIdentity,
            ),
            "bot-a",
            "telegram",
            "telegram-update-1",
        );
        let channel_context = ChannelContext {
            bus: runtime.bus(),
            sessions: runtime.session_store.clone(),
            ui: Some(runtime.ui_service.clone()),
            readiness: None,
        };
        let platform_session = sylvander_channel::authorize_external_chat(
            &channel_context,
            &platform_boundary,
            sylvander_channel::ExternalChatRequest {
                existing_session: None,
                agent_id: AgentId::new("assistant"),
                label: "telegram-42".into(),
                overrides: SessionConfigOverrides::default(),
                text: "hello from Telegram".into(),
                attachments: Vec::new(),
                external_meta: std::collections::BTreeMap::from([
                    ("channel_instance_id".into(), "bot-a".into()),
                    ("chat_id".into(), "42".into()),
                ]),
            },
        )
        .await
        .expect("an allowed platform principal may create and use its session");
        let platform_stored = runtime
            .session_store
            .get(&platform_session)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(platform_stored.metadata.user_id, "telegram:bot-a:42");
        assert_eq!(
            platform_stored.external_meta["channel_instance_id"],
            "bot-a"
        );
        assert!(platform_stored.effective_config.is_some());
        let other_bot = sylvander_protocol::BoundaryContext::authenticated(
            sylvander_protocol::AuthenticatedPrincipal::user(
                "telegram:bot-b:42",
                sylvander_protocol::AuthenticationMethod::PlatformIdentity,
            ),
            "bot-b",
            "telegram",
            "telegram-update-2",
        );
        let denial = sylvander_channel::authorize_external_chat(
            &channel_context,
            &other_bot,
            sylvander_channel::ExternalChatRequest {
                existing_session: Some(platform_session),
                agent_id: AgentId::new("assistant"),
                label: "telegram-42".into(),
                overrides: SessionConfigOverrides::default(),
                text: "cross-instance attempt".into(),
                attachments: Vec::new(),
                external_meta: std::collections::BTreeMap::new(),
            },
        )
        .await
        .expect_err("another channel instance must not reuse the session");
        assert_eq!(
            denial.code,
            sylvander_protocol::BoundaryErrorCode::Forbidden
        );
        let stranger = sylvander_protocol::BoundaryContext::authenticated(
            sylvander_protocol::AuthenticatedPrincipal::user(
                "other-user",
                sylvander_protocol::AuthenticationMethod::UnixPeer,
            ),
            "tui-local",
            "unix",
            "request-read",
        );
        assert!(
            sylvander_channel::UiService::discover_agents(runtime.ui_service.as_ref(), &stranger,)
                .await
                .unwrap()
                .is_empty()
        );
        let denial = sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &stranger,
            &sylvander_protocol::UiClientMessage::CreateSession {
                request: SessionCreateRequest {
                    agent_id: AgentId::new("assistant"),
                    label: "unauthorized".into(),
                    channel_id: Some("tui-local".into()),
                    overrides: SessionConfigOverrides::default(),
                },
            },
        )
        .await
        .expect_err("an Agent allowlist must be enforced before creation");
        assert_eq!(
            denial.code,
            sylvander_protocol::BoundaryErrorCode::Forbidden
        );
        let denial = sylvander_channel::UiService::session_config(
            runtime.ui_service.as_ref(),
            &stranger,
            &created.session_id,
        )
        .await
        .expect_err("a different principal must not read the session");
        assert_eq!(
            denial.code,
            sylvander_protocol::BoundaryErrorCode::Forbidden
        );
        let chat_denial = sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &stranger,
            &sylvander_protocol::UiClientMessage::Chat {
                text: "cross-session attempt".into(),
                attachments: Vec::new(),
                session_id: Some(created.session_id.0.clone()),
                workspace: None,
            },
        )
        .await
        .expect_err("message dispatch must enforce the same ownership boundary");
        assert_eq!(
            chat_denial.code,
            sylvander_protocol::BoundaryErrorCode::Forbidden
        );
        let unauthenticated = sylvander_protocol::BoundaryContext::unauthenticated(
            "websocket",
            "websocket",
            "request-ping",
        );
        let denial = sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &unauthenticated,
            &sylvander_protocol::UiClientMessage::Ping,
        )
        .await
        .expect_err("an unauthenticated transport must fail closed");
        assert_eq!(
            denial.code,
            sylvander_protocol::BoundaryErrorCode::Unauthenticated
        );
        let authentication_boundary = sylvander_protocol::BoundaryContext::unauthenticated(
            "websocket",
            "websocket",
            "request-authentication-failure",
        );
        let authentication_denial = sylvander_channel::UiService::reject_authentication(
            runtime.ui_service.as_ref(),
            &authentication_boundary,
            sylvander_protocol::AuthenticationFailure::new(
                sylvander_protocol::AuthenticationMethod::BearerToken,
            ),
        )
        .await;
        assert_eq!(
            authentication_denial.code,
            sylvander_protocol::BoundaryErrorCode::Unauthenticated
        );
        assert!(
            runtime
                .engine
                .get_session(&created.session_id)
                .await
                .is_some()
        );
        let evidence = runtime
            .evidence_store()
            .expect("evidence enabled by default");
        evidence
            .start_run("feedback-auth-run".into(), "test".into(), 10)
            .await
            .unwrap();
        evidence
            .start_turn(crate::evidence::TurnStart {
                id: "feedback-auth-turn".into(),
                run_id: "feedback-auth-run".into(),
                session_id: created.session_id.0.clone(),
                agent_id: Some("assistant".into()),
                started_at: 11,
                input_bytes: 0,
                input_digest: None,
            })
            .await
            .unwrap();
        let feedback_message = sylvander_protocol::UiClientMessage::SubmitFeedback {
            feedback: RunFeedback {
                run_id: "feedback-auth-run".into(),
                turn_id: Some("feedback-auth-turn".into()),
                rating: sylvander_protocol::FeedbackRating::Positive,
                note: None,
                tags: Vec::new(),
            },
        };
        sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &owner,
            &feedback_message,
        )
        .await
        .expect("the session owner may submit feedback");
        let denial = sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &stranger,
            &feedback_message,
        )
        .await
        .expect_err("another principal must not submit feedback for the turn");
        assert_eq!(
            denial.code,
            sylvander_protocol::BoundaryErrorCode::Forbidden
        );
        evidence
            .finish_run("feedback-auth-run".into(), 12, "succeeded")
            .await
            .unwrap();
        let denials = evidence.authorization_denials(10).await.unwrap();
        assert_eq!(denials.len(), 8);
        let authentication_audit = denials
            .iter()
            .find(|denial| denial.operation == "authenticate_bearer_token")
            .expect("authentication rejection must be audited by the runtime");
        assert!(authentication_audit.principal_digest.is_none());
        assert!(authentication_audit.resource_digest.is_none());
        assert!(denials.iter().all(|denial| denial.principal_digest.is_some()
            || denial.code == "unauthenticated"));
        assert!(
            denials
                .iter()
                .all(|denial| denial.resource_digest.as_deref()
                    != Some(created.session_id.0.as_str()))
        );
        let original_revision = restart_config.agents[0].revision;
        let mut next_definition = restart_config.agents[0].clone();
        next_definition.revision += 1;
        next_definition.spec.name = "Sylvander revised".into();
        next_definition.spec.model.model_name = "model-b".into();
        next_definition.spec.persona.system_prompt = "revision two prompt".into();
        next_definition.access = crate::config::AgentAccessConfig::default();
        let administrator = sylvander_protocol::BoundaryContext::authenticated(
            sylvander_protocol::AuthenticatedPrincipal {
                id: sylvander_protocol::PrincipalId::new("operator"),
                kind: sylvander_protocol::PrincipalKind::User,
                authentication: sylvander_protocol::AuthenticationMethod::Internal,
                roles: vec!["admin".into()],
            },
            "admin-console",
            "internal",
            "hot-activate",
        );
        let mut uncomposable = next_definition.clone();
        uncomposable.prompt_profiles = vec![crate::config::PromptProfileConfig {
            id: "wrong-provider".into(),
            providers: vec!["another-provider".into()],
            models: Vec::new(),
            system_prompt: "must not persist".into(),
        }];
        uncomposable.default_prompt_profile = Some("wrong-provider".into());
        let rejected = sylvander_channel::UiService::agent_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::AgentAdminRequest::UpdateDefinition {
                expected_active_revision: original_revision,
                definition: Box::new(
                    crate::agent_admin::draft_from_definition(&uncomposable).unwrap(),
                ),
            },
        )
        .await;
        assert!(
            matches!(
                rejected,
                sylvander_protocol::AgentAdminResponse::Error {
                    error: sylvander_protocol::AgentAdminError {
                        code: sylvander_protocol::AgentAdminErrorCode::InvalidDefinition,
                        ..
                    }
                }
            ),
            "unexpected rejection response: {rejected:?}"
        );
        let inspected = sylvander_channel::UiService::agent_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::AgentAdminRequest::ListRevisions {
                agent_id: next_definition.spec.id.clone(),
                before_revision: None,
                limit: 10,
            },
        )
        .await;
        assert!(matches!(
            inspected,
            sylvander_protocol::AgentAdminResponse::Success { result }
                if matches!(
                    result.as_ref(),
                    sylvander_protocol::AgentAdminResult::RevisionsListed {
                        active_revision,
                        revisions,
                        ..
                    } if *active_revision == original_revision && revisions.len() == 1
                )
        ));
        let update_request = sylvander_protocol::AgentAdminRequest::UpdateDefinition {
            expected_active_revision: original_revision,
            definition: Box::new(
                crate::agent_admin::draft_from_definition(&next_definition).unwrap(),
            ),
        };
        let update_message = sylvander_protocol::UiClientMessage::AgentAdmin {
            request: update_request.clone(),
        };
        let denial = sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &owner,
            &update_message,
        )
        .await
        .expect_err("ordinary session owners must not administer Agents");
        assert_eq!(
            denial.code,
            sylvander_protocol::BoundaryErrorCode::Forbidden
        );
        sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &administrator,
            &update_message,
        )
        .await
        .expect("administrators may reach the Agent administration service");
        let registry_request = sylvander_protocol::RegistryAdminRequest::InspectProviderRevision {
            provider_id: "primary".into(),
            revision: 1,
        };
        let registry_message = sylvander_protocol::UiClientMessage::RegistryAdmin {
            request: registry_request.clone(),
        };
        assert!(
            sylvander_channel::UiService::authorize_message(
                runtime.ui_service.as_ref(),
                &owner,
                &registry_message,
            )
            .await
            .is_err()
        );
        sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &administrator,
            &registry_message,
        )
        .await
        .expect("administrators may reach the registry administration seam");
        let unauthorized_registry = sylvander_channel::UiService::registry_admin(
            runtime.ui_service.as_ref(),
            &owner,
            registry_request.clone(),
        )
        .await;
        assert!(matches!(
            unauthorized_registry,
            sylvander_protocol::RegistryAdminResponse::Error { error }
                if error.code == sylvander_protocol::RegistryAdminErrorCode::Unauthorized
        ));
        let inspected_provider = sylvander_channel::UiService::registry_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            registry_request,
        )
        .await;
        assert!(matches!(
            inspected_provider,
            sylvander_protocol::RegistryAdminResponse::Success { result }
                if matches!(
                    result.as_ref(),
                    sylvander_protocol::RegistryAdminResult::ProviderRevisionInspected {
                        revision
                    } if revision.definition.provider_id == "primary"
                        && revision.definition.revision == 1
                )
        ));
        let missing_provider_revision = sylvander_channel::UiService::registry_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::RegistryAdminRequest::InspectProviderRevision {
                provider_id: "primary".into(),
                revision: 99,
            },
        )
        .await;
        assert!(matches!(
            missing_provider_revision,
            sylvander_protocol::RegistryAdminResponse::Error { error }
                if error.code == sylvander_protocol::RegistryAdminErrorCode::UnknownRevision
        ));
        let binding_id = "credential/runtime-audit";
        let create_credential = sylvander_channel::UiService::registry_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::RegistryAdminRequest::CreateCredentialBinding {
                binding_id: binding_id.into(),
                reference: sylvander_protocol::CredentialSecretReferenceDraft::File {
                    path: secret.display().to_string(),
                },
            },
        )
        .await;
        assert!(matches!(
            create_credential,
            sylvander_protocol::RegistryAdminResponse::Success { .. }
        ));
        let stage_credential = sylvander_channel::UiService::registry_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::RegistryAdminRequest::StageCredentialGeneration {
                binding_id: binding_id.into(),
                generation: 2,
                expected_active_generation: 1,
                reference: sylvander_protocol::CredentialSecretReferenceDraft::File {
                    path: secret.display().to_string(),
                },
            },
        )
        .await;
        assert!(matches!(
            stage_credential,
            sylvander_protocol::RegistryAdminResponse::Success { .. }
        ));
        let activate_credential = sylvander_channel::UiService::registry_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::RegistryAdminRequest::ActivateCredentialGeneration {
                binding_id: binding_id.into(),
                generation: 2,
                expected_active_generation: 1,
            },
        )
        .await;
        assert!(matches!(
            activate_credential,
            sylvander_protocol::RegistryAdminResponse::Success { .. }
        ));
        let conflict = sylvander_channel::UiService::registry_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::RegistryAdminRequest::RollbackCredentialGeneration {
                binding_id: binding_id.into(),
                target_generation: 1,
                expected_active_generation: 1,
            },
        )
        .await;
        assert!(matches!(
            conflict,
            sylvander_protocol::RegistryAdminResponse::Error { error }
                if error.code
                    == sylvander_protocol::RegistryAdminErrorCode::ActiveGenerationConflict
        ));
        let rollback_credential = sylvander_channel::UiService::registry_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::RegistryAdminRequest::RollbackCredentialGeneration {
                binding_id: binding_id.into(),
                target_generation: 1,
                expected_active_generation: 2,
            },
        )
        .await;
        assert!(matches!(
            rollback_credential,
            sylvander_protocol::RegistryAdminResponse::Success { .. }
        ));
        let registry_audits = evidence.administration_audits(20).await.unwrap();
        assert!(registry_audits.iter().any(|audit| {
            audit.operation == "inspect_provider_revision"
                && audit.resource_kind == "provider"
                && audit.resource_digest != "primary"
                && audit.version == Some(1)
                && audit.outcome == "succeeded"
        }));
        assert!(registry_audits.iter().any(|audit| {
            audit.operation == "inspect_provider_revision"
                && audit.version == Some(99)
                && audit.outcome == "failed"
                && audit.error_code.as_deref() == Some("unknown_revision")
        }));
        for (operation, version, outcome) in [
            ("create_credential_binding", 1, "succeeded"),
            ("stage_credential_generation", 2, "succeeded"),
            ("activate_credential_generation", 2, "succeeded"),
            ("rollback_credential_generation", 1, "succeeded"),
        ] {
            assert!(registry_audits.iter().any(|audit| {
                audit.operation == operation
                    && audit.resource_kind == "credential"
                    && audit.resource_digest != binding_id
                    && audit.version == Some(version)
                    && audit.outcome == outcome
            }));
        }
        assert!(registry_audits.iter().any(|audit| {
            audit.operation == "rollback_credential_generation"
                && audit.version == Some(1)
                && audit.outcome == "failed"
                && audit.error_code.as_deref() == Some("active_generation_conflict")
        }));
        assert!(
            registry_audits
                .iter()
                .all(|audit| audit.outcome != "pending")
        );
        let admin_denials = evidence.authorization_denials(20).await.unwrap();
        assert!(
            admin_denials
                .iter()
                .any(|denial| denial.operation == "agent_admin")
        );
        assert!(
            admin_denials
                .iter()
                .any(|denial| denial.operation == "registry_admin")
        );
        let updated = sylvander_channel::UiService::agent_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            update_request,
        )
        .await;
        assert!(matches!(
            updated,
            sylvander_protocol::AgentAdminResponse::Success { result }
                if matches!(
                    result.as_ref(),
                    sylvander_protocol::AgentAdminResult::DefinitionUpdated { revision }
                        if revision.definition.revision == next_definition.revision
                            && !revision.active
                )
        ));
        let activated = sylvander_channel::UiService::agent_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::AgentAdminRequest::ActivateRevision {
                agent_id: next_definition.spec.id.clone(),
                revision: next_definition.revision,
                expected_active_revision: original_revision,
            },
        )
        .await;
        assert!(matches!(
            activated,
            sylvander_protocol::AgentAdminResponse::Success { result }
                if matches!(
                    result.as_ref(),
                    sylvander_protocol::AgentAdminResult::RevisionActivated {
                        active_revision,
                        ..
                    } if *active_revision == next_definition.revision
                )
        ));
        let discovered = sylvander_channel::UiService::discover_agents(
            runtime.ui_service.as_ref(),
            &administrator,
        )
        .await
        .unwrap();
        assert_eq!(discovered[0].revision, next_definition.revision);
        assert_eq!(discovered[0].name, next_definition.spec.name);
        let activated_session = sylvander_channel::UiService::create_session(
            runtime.ui_service.as_ref(),
            &administrator,
            SessionCreateRequest {
                agent_id: next_definition.spec.id.clone(),
                label: "hot activated revision".into(),
                channel_id: Some("admin-console".into()),
                overrides: SessionConfigOverrides::default(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            activated_session.effective.agent_revision, next_definition.revision,
            "new sessions must bind the hot-activated revision"
        );
        let provider = runtime.revision_provider.as_ref().unwrap();
        let original_run = provider
            .configured_revision(&next_definition.spec.id, original_revision)
            .await
            .unwrap()
            .run;
        let activated_run = provider
            .configured_revision(&next_definition.spec.id, next_definition.revision)
            .await
            .unwrap()
            .run;
        tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
            loop {
                if original_run
                    .get_session(&created.session_id)
                    .await
                    .is_some()
                    && activated_run
                        .get_session(&activated_session.session_id)
                        .await
                        .is_some()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("revision workers must receive only their bound sessions");
        assert!(
            activated_run
                .get_session(&created.session_id)
                .await
                .is_none(),
            "an existing session must not drift to the activated revision"
        );
        let mut original_probe = sylvander_protocol::BusMessage::user_chat(
            created.session_id.clone(),
            "operator",
            "revision-one-probe",
        );
        original_probe.recipient =
            sylvander_protocol::Recipient::Agent(next_definition.spec.id.clone());
        runtime.bus().publish(original_probe).await.unwrap();
        let mut activated_probe = sylvander_protocol::BusMessage::user_chat(
            activated_session.session_id.clone(),
            "operator",
            "revision-two-probe",
        );
        activated_probe.recipient =
            sylvander_protocol::Recipient::Agent(next_definition.spec.id.clone());
        runtime.bus().publish(activated_probe).await.unwrap();
        let revision_requests = tokio::time::timeout(tokio::time::Duration::from_secs(2), async {
            loop {
                let observed = model_server
                    .received_requests()
                    .await
                    .unwrap()
                    .into_iter()
                    .filter_map(|request| {
                        let body: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
                        let encoded = body.to_string();
                        let probe = ["revision-one-probe", "revision-two-probe"]
                            .into_iter()
                            .find(|probe| encoded.contains(probe))?;
                        let model = body.get("model")?.as_str()?.to_owned();
                        let prompt = body
                            .get("system")?
                            .as_array()?
                            .first()?
                            .get("text")?
                            .as_str()?
                            .to_owned();
                        Some((probe.to_owned(), model, prompt))
                    })
                    .collect::<Vec<_>>();
                if observed.len() == 2 {
                    break observed;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both revision-bound requests must reach the model provider");
        assert!(revision_requests.contains(&(
            "revision-one-probe".into(),
            "model-a".into(),
            "revision one prompt".into(),
        )));
        assert!(revision_requests.contains(&(
            "revision-two-probe".into(),
            "model-b".into(),
            "revision two prompt".into(),
        )));

        let stale_activation = sylvander_channel::UiService::agent_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::AgentAdminRequest::ActivateRevision {
                agent_id: next_definition.spec.id.clone(),
                revision: original_revision,
                expected_active_revision: original_revision,
            },
        )
        .await;
        assert!(matches!(
            stale_activation,
            sylvander_protocol::AgentAdminResponse::Error {
                error: sylvander_protocol::AgentAdminError {
                    code: sylvander_protocol::AgentAdminErrorCode::RevisionConflict,
                    ..
                }
            }
        ));
        let after_conflict = sylvander_channel::UiService::discover_agents(
            runtime.ui_service.as_ref(),
            &administrator,
        )
        .await
        .unwrap();
        assert_eq!(
            after_conflict[0].revision, next_definition.revision,
            "an optimistic conflict must not move the active revision"
        );

        let rolled_back = sylvander_channel::UiService::agent_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::AgentAdminRequest::RollbackRevision {
                agent_id: next_definition.spec.id.clone(),
                target_revision: original_revision,
                expected_active_revision: next_definition.revision,
            },
        )
        .await;
        assert!(matches!(
            rolled_back,
            sylvander_protocol::AgentAdminResponse::Success { result }
                if matches!(
                    result.as_ref(),
                    sylvander_protocol::AgentAdminResult::RevisionRolledBack {
                        active_revision,
                        ..
                    } if *active_revision == original_revision
                )
        ));
        let rolled_back_session = sylvander_channel::UiService::create_session(
            runtime.ui_service.as_ref(),
            &administrator,
            SessionCreateRequest {
                agent_id: next_definition.spec.id.clone(),
                label: "hot rolled back revision".into(),
                channel_id: Some("admin-console".into()),
                overrides: SessionConfigOverrides::default(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            rolled_back_session.effective.agent_revision, original_revision,
            "rollback must affect new sessions without restarting"
        );
        let reactivated = sylvander_channel::UiService::agent_admin(
            runtime.ui_service.as_ref(),
            &administrator,
            sylvander_protocol::AgentAdminRequest::ActivateRevision {
                agent_id: next_definition.spec.id.clone(),
                revision: next_definition.revision,
                expected_active_revision: original_revision,
            },
        )
        .await;
        assert!(matches!(
            reactivated,
            sylvander_protocol::AgentAdminResponse::Success { result }
                if matches!(
                    result.as_ref(),
                    sylvander_protocol::AgentAdminResult::RevisionActivated {
                        active_revision,
                        ..
                    } if *active_revision == next_definition.revision
                )
        ));
        let administration_audits = evidence.agent_administration_audits(10).await.unwrap();
        assert_eq!(administration_audits.len(), 6);
        assert!(
            administration_audits
                .iter()
                .all(|audit| audit.principal_digest != "operator"
                    && audit.agent_digest != "assistant")
        );
        assert_eq!(
            administration_audits
                .iter()
                .filter(|audit| audit.outcome == "succeeded")
                .count(),
            4
        );
        assert_eq!(
            administration_audits
                .iter()
                .filter(|audit| audit.outcome == "failed")
                .count(),
            2
        );
        assert!(administration_audits.iter().any(|audit| {
            audit.operation == "activate_revision"
                && audit.revision == original_revision
                && audit.expected_active_revision == original_revision
                && audit.outcome == "failed"
                && audit.error_code.as_deref()
                    == Some(
                        agent_admin_error_code(
                            sylvander_protocol::AgentAdminErrorCode::RevisionConflict,
                        )
                        .as_str(),
                    )
        }));
        let owner_denial = sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &owner,
            &sylvander_protocol::UiClientMessage::GetSessionConfig {
                session_id: created.session_id.0.clone(),
            },
        )
        .await
        .expect_err("activating a restrictive Agent policy must revoke existing access");
        assert_eq!(
            owner_denial.code,
            sylvander_protocol::BoundaryErrorCode::Forbidden
        );
        assert!(
            sylvander_channel::UiService::discover_agents(runtime.ui_service.as_ref(), &owner)
                .await
                .unwrap()
                .is_empty()
        );
        let direct_denial = sylvander_channel::UiService::session_config(
            runtime.ui_service.as_ref(),
            &owner,
            &created.session_id,
        )
        .await
        .expect_err("direct session reads must enforce the active Agent policy");
        assert_eq!(
            direct_denial.code,
            sylvander_protocol::BoundaryErrorCode::Forbidden
        );
        let feedback_denial = sylvander_channel::UiService::submit_feedback(
            runtime.ui_service.as_ref(),
            &owner,
            RunFeedback {
                run_id: "feedback-auth-run".into(),
                turn_id: Some("feedback-auth-turn".into()),
                rating: sylvander_protocol::FeedbackRating::Positive,
                note: None,
                tags: Vec::new(),
            },
        )
        .await
        .expect_err("direct feedback writes must enforce the active Agent policy");
        assert_eq!(
            feedback_denial.code,
            sylvander_protocol::BoundaryErrorCode::Forbidden
        );

        for principal in [
            sylvander_protocol::AuthenticatedPrincipal {
                id: sylvander_protocol::PrincipalId::new("operator"),
                kind: sylvander_protocol::PrincipalKind::User,
                authentication: sylvander_protocol::AuthenticationMethod::Internal,
                roles: vec!["admin".into()],
            },
            sylvander_protocol::AuthenticatedPrincipal {
                id: sylvander_protocol::PrincipalId::new("runtime"),
                kind: sylvander_protocol::PrincipalKind::System,
                authentication: sylvander_protocol::AuthenticationMethod::Internal,
                roles: Vec::new(),
            },
        ] {
            let privileged = sylvander_protocol::BoundaryContext::authenticated(
                principal,
                "internal-control",
                "internal",
                uuid::Uuid::new_v4().to_string(),
            );
            sylvander_channel::UiService::authorize_message(
                runtime.ui_service.as_ref(),
                &privileged,
                &sylvander_protocol::UiClientMessage::GetSessionConfig {
                    session_id: created.session_id.0.clone(),
                },
            )
            .await
            .expect("admin and system principals retain emergency access");
        }
        runtime.shutdown().await.unwrap();
        let counts = evidence.counts().await.unwrap();
        assert_eq!(counts.runs, 2);
        assert!(counts.events >= 1, "Agent lifecycle must reach evidence");

        let restarted = Runtime::boot_config(restart_config).await.unwrap();
        assert_eq!(
            restarted
                .configured_agent(&AgentId::new("assistant"))
                .unwrap()
                .definition
                .revision,
            next_definition.revision
        );
        let preserved = restarted
            .session_store
            .get(&created.session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            preserved.effective_config.unwrap().agent_revision,
            original_revision,
            "activation must not migrate an existing session"
        );
        let (_, updated) = restarted
            .update_session_config(
                &created.session_id,
                preserved.config_revision,
                SessionConfigOverrides::default(),
            )
            .await
            .unwrap();
        assert_eq!(updated.agent_revision, original_revision);
        restarted.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn create_ephemeral_session_with_external_meta() {
        let config = SystemConfig {
            name: "test-runtime".into(),
            agents: vec![test_spec("agent-1")],
            sessions: vec![],
        };

        let rt = Runtime::boot(config, test_client()).await.expect("boot");

        let mut meta = HashMap::new();
        meta.insert("chat_id".into(), serde_json::json!("-100xxx"));

        let sid = rt
            .create_ephemeral_session(
                "ephemeral",
                test_metadata(),
                &[AgentId::new("agent-1")],
                meta,
            )
            .await
            .expect("create");

        let stored = rt.get_external_meta(&sid).await.expect("should exist");
        assert_eq!(
            stored.get("chat_id").unwrap(),
            &serde_json::json!("-100xxx")
        );

        rt.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn external_meta_not_in_engine() {
        // Protocol metadata stays in the runtime layer — the engine
        // (and agents) never see it.
        let config = SystemConfig {
            name: "test-runtime".into(),
            agents: vec![test_spec("agent-1")],
            sessions: vec![],
        };

        let rt = Runtime::boot(config, test_client()).await.expect("boot");

        let mut meta = HashMap::new();
        meta.insert("secret".into(), serde_json::json!("hidden"));

        rt.create_ephemeral_session("test", test_metadata(), &[AgentId::new("agent-1")], meta)
            .await
            .expect("create");

        // Engine sessions have no external_meta field
        let engine_sessions = rt.engine.list_sessions().await;
        assert_eq!(engine_sessions.len(), 1);
        // SessionMeta (engine-level) has no external_meta

        rt.shutdown().await.expect("shutdown");
    }
}
