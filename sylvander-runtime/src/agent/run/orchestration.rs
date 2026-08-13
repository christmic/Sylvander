//! Event-driven orchestration for one Runtime-owned Agent turn.

use std::path::Path;
use std::sync::Arc;

use futures_util::StreamExt;
use tokio::sync::oneshot;
use tracing::{Instrument as _, info};

use sylvander_agent::approval::{ApprovalDecision, ApprovalGate};
use sylvander_agent::ask_user_gate::AskUserGate;
use sylvander_agent::execution_ports::AgentExecutionPorts;
use sylvander_agent::kernel::agent_loop;
use sylvander_agent::memory::curated::{CuratedContextSubject, CuratedMemoryScope};
use sylvander_agent::memory::store::MemoryExecutionContext;
use sylvander_agent::plan_gate::{PlanDecision, PlanGate};
use sylvander_agent::prompt::SHARED_SAFETY_PROMPT;
use sylvander_agent::task_gate::TaskGate;
use sylvander_agent::tool::ToolFailureKind as AgentToolFailureKind;
use sylvander_agent::tool_context::{Cap, NetworkPolicy, ToolContext};
use sylvander_agent::tools::{MemoryReadTool, ReadTool};
use sylvander_agent::turn::conversation::ConversationSnapshot;
use sylvander_agent::turn::error::AgentLoopError;
use sylvander_agent::turn::execution_context::{AgentExecutionContext, ExecutionWorkspace};
use sylvander_agent::turn::identity::{
    AgentId as KernelAgentId, SessionId as KernelSessionId, UserId as KernelUserId,
};
use sylvander_agent::turn::request::AgentTurnRequest;
use sylvander_agent::turn_context::{
    TurnContextCandidate, TurnContextInputs, TurnContextLayerKind, TurnContextProvenance,
    TurnContextSource, compose_turn_context, retrieve_relationship_context,
    retrieve_workspace_context,
};
use sylvander_agent::workspace_executor::{
    MountedWorkspace, UnavailableExecutor, WorkspaceCapabilities, WorkspaceExecutor,
    WorkspaceRouter, WorkspaceTarget,
};
use sylvander_agent::workspace_journal::WorkspaceMutationJournal;
use sylvander_api::{BusMessage, Sender};
use sylvander_llm_core::{
    ChatMessage, ContentBlock, ImageContent, MediaSource, ModelResponse, ReasoningConfig,
    ReasoningEffort as ProviderReasoningEffort,
};

use super::background::BusTaskGate;
use super::error::prompt_integrity_error;
use super::interaction::{
    BusApprovalGate, BusAskUserGate, BusPlanGate, DenyAllApprovalGate, publish_interaction_timeout,
};
use super::projection::{
    public_compaction_report, public_retry_cause, runtime_failure_kind,
    runtime_persistence_operation, turn_failure_kind, usage_cost_nano_usd,
};
use super::workspace_context;
use super::{
    AgentRunError, AgentRunInner, ContextUsage, RuntimeTurnSnapshot, SessionPersistenceOperation,
    turn_system_instructions, validate_tool_gateway_surface,
};
use crate::agent::approval::{ApprovalGrantContext, approval_policy_revision};
use crate::agent_definition::{AgentId, SessionId};
use crate::execution::RuntimeExecutionService;
use crate::observability::{RuntimeEvent, RuntimePersistenceOperation, RuntimeToolFailureKind};
use crate::prompt_contract::{agent_model_selection, public_prompt_manifest};
use crate::session::{SessionMetadata, now_secs};
use crate::storage::artifact::ArtifactTurnBinding;
use crate::storage::session::{
    SessionStoreError, ToolCallAdvance, ToolCallCompletion,
    ToolCallFailureKind as StoredToolCallFailureKind, ToolCallStart, ToolCallState,
    ToolExecutionPosition, ToolInvocationId, ToolResultPersistence, TurnCompletion, TurnStart,
    TurnState,
};
use crate::storage::workspace_journal::WorkspaceJournal;

impl AgentRunInner {
    pub(super) async fn interrupt_turn(&self, session_id: &SessionId) {
        if let Some(turn) = self.active_turns.lock().await.remove(session_id) {
            let _ = turn.interrupt.send(());
        }
    }

    async fn cancel_pending_decisions(&self, session_id: &SessionId) {
        let approval_ids = {
            let pending = self.pending_approvals.lock().await;
            pending
                .iter()
                .filter(|(_, request)| &request.session_id == session_id)
                .map(|(call_id, _)| call_id.clone())
                .collect::<Vec<_>>()
        };
        let mut approvals = self.pending_approvals.lock().await;
        for call_id in approval_ids {
            if let Some(request) = approvals.remove(&call_id) {
                let _ = request.sender.send(ApprovalDecision::Rejected {
                    reason: "turn interrupted by user".into(),
                });
            }
        }
        drop(approvals);

        let answer_ids = {
            let pending = self.pending_answers.lock().await;
            pending
                .iter()
                .filter(|(_, request)| &request.session_id == session_id)
                .map(|(call_id, _)| call_id.clone())
                .collect::<Vec<_>>()
        };
        let mut answers = self.pending_answers.lock().await;
        for call_id in answer_ids {
            if let Some(request) = answers.remove(&call_id) {
                let _ = request.sender.send(Vec::new());
            }
        }
        drop(answers);

        let plan_ids = {
            let pending = self.pending_plans.lock().await;
            pending
                .iter()
                .filter(|(_, request)| &request.session_id == session_id)
                .map(|(plan_id, _)| plan_id.clone())
                .collect::<Vec<_>>()
        };
        let mut plans = self.pending_plans.lock().await;
        for plan_id in plan_ids {
            if let Some(request) = plans.remove(&plan_id) {
                let _ = request.sender.send(PlanDecision::Rejected {
                    reason: "turn interrupted by user".into(),
                });
            }
        }
    }

    /// Core: handle a chat message. Runs the loop with streaming.
    pub(super) async fn handle_message(&self, msg: BusMessage) -> Result<(), AgentRunError> {
        self.handle_message_correlated(msg, std::future::pending::<()>(), uuid::Uuid::new_v4())
            .await
    }

    pub(super) async fn handle_message_interruptible(
        &self,
        msg: BusMessage,
        interrupted: oneshot::Receiver<()>,
        turn_id: uuid::Uuid,
    ) -> Result<(), AgentRunError> {
        self.handle_message_correlated(msg, interrupted, turn_id)
            .await
    }

    async fn handle_message_correlated<F>(
        &self,
        msg: BusMessage,
        interrupted: F,
        turn_id: uuid::Uuid,
    ) -> Result<(), AgentRunError>
    where
        F: std::future::Future,
    {
        let correlation = TurnCorrelation::new(&msg, turn_id);
        let session_id = msg.session_id.clone();
        let span = tracing::info_span!(
            "agent_turn",
            agent_id = %self.id,
            session_id = %msg.session_id,
            turn_id = %correlation.turn,
            request_id = %correlation.request,
            trace_id = %correlation.trace,
        );
        self.observability.record(RuntimeEvent::TurnStarted {
            request_id: correlation.request.clone(),
            trace_id: correlation.trace.clone(),
            turn_id: correlation.turn.clone(),
            session_id: session_id.clone(),
            agent_id: self.id.clone(),
        });
        async {
            info!("turn started");
            let result = self
                .handle_message_with_interrupt(msg, interrupted, &correlation.turn)
                .await;
            if let Err(error) = &result {
                if let Some(store) = &self.session_store {
                    let persisted = match store.turn(&session_id, &correlation.turn).await {
                        Ok(Some(turn)) if turn.state == TurnState::Running => Some(
                            store
                                .finish_turn(
                                    &session_id,
                                    &correlation.turn,
                                    TurnState::Failed,
                                    Some(turn_failure_kind(error)),
                                )
                                .await,
                        ),
                        Ok(_) => None,
                        Err(source) => Some(Err(source)),
                    };
                    if let Some(persisted) = persisted {
                        self.observability
                            .record(RuntimeEvent::PersistenceFinished {
                                turn_id: correlation.turn.clone(),
                                session_id: session_id.clone(),
                                operation: RuntimePersistenceOperation::FinishTurn,
                                succeeded: persisted.is_ok(),
                            });
                    }
                }
                if let AgentRunError::SessionPersistence { operation, .. } = error {
                    self.observability
                        .record(RuntimeEvent::PersistenceFinished {
                            turn_id: correlation.turn.clone(),
                            session_id: session_id.clone(),
                            operation: runtime_persistence_operation(*operation),
                            succeeded: false,
                        });
                }
                self.observability.record(RuntimeEvent::TurnFailed {
                    turn_id: correlation.turn.clone(),
                    session_id: session_id.clone(),
                    kind: runtime_failure_kind(error),
                });
                match error {
                    AgentRunError::Loop(error) => self.publish_error(&session_id, error).await,
                    AgentRunError::SessionPersistence { .. } => {
                        self.publish_stream(
                            &session_id,
                            sylvander_api::StreamEvent::Error {
                                message: error.to_string(),
                            },
                        )
                        .await;
                    }
                    AgentRunError::UnknownSession(_)
                    | AgentRunError::Authentication(_)
                    | AgentRunError::Build(_)
                    | AgentRunError::Configuration(_) => {}
                }
            }
            self.turn_snapshots.write().await.remove(&session_id);
            info!(succeeded = result.is_ok(), "turn finished");
            result
        }
        .instrument(span)
        .await
    }

    async fn handle_message_with_interrupt<F>(
        &self,
        msg: BusMessage,
        interrupted: F,
        turn_id: &str,
    ) -> Result<(), AgentRunError>
    where
        F: std::future::Future,
    {
        let session_id = msg.session_id.clone();
        let user_message = Self::message_to_param(&msg);
        let stored_session = if let Some(store) = &self.session_store {
            store.get(&session_id).await.map_err(|source| {
                AgentRunError::session_persistence(
                    SessionPersistenceOperation::InspectSession,
                    source,
                )
            })?
        } else {
            None
        };
        if let (Some(stored), Sender::User(sender)) = (&stored_session, &msg.sender)
            && sender != &stored.metadata.user_id
        {
            return Err(AgentRunError::Configuration(
                "session identity verification failed".into(),
            ));
        }
        let effective_config = stored_session
            .as_ref()
            .map(|session| {
                session.effective_config.clone().ok_or_else(|| {
                    AgentRunError::Configuration(format!(
                        "durable session {session_id} has no effective configuration"
                    ))
                })
            })
            .transpose()?;
        if let Some(effective) = &effective_config
            && effective.agent_id != self.id
        {
            return Err(AgentRunError::Configuration(format!(
                "session {session_id} is configured for Agent {}, not {}",
                effective.agent_id, self.id
            )));
        }
        let (selected_model, selected_effort) = {
            let runtime = self.runtime_models.read().await;
            let selection = effective_config.as_ref().map_or_else(
                || runtime.current.clone(),
                |config| sylvander_api::ModelSelection {
                    provider_id: config.provider_id.clone(),
                    model_id: config.model_id.clone(),
                },
            );
            let model = runtime
                .available
                .get(&selection)
                .ok_or_else(|| {
                    AgentRunError::Configuration(format!(
                        "session {session_id} selects unavailable model `{}/{}`",
                        selection.provider_id, selection.model_id
                    ))
                })?
                .clone();
            (
                model,
                effective_config
                    .as_ref()
                    .map_or(runtime.reasoning_effort, |config| config.reasoning_effort),
            )
        };
        let selected_exact_model = Self::validate_turn_model(&selected_model, selected_effort)?;
        let loop_config = self.loop_config.clone();
        let selected_pricing = selected_model.pricing;
        let session_metadata = {
            let sessions = self.sessions.read().await;
            let ctx = sessions
                .get(&session_id)
                .ok_or_else(|| AgentRunError::UnknownSession(session_id.clone()))?;
            ctx.metadata.clone()
        };
        let user_profile = self
            .load_user_profile(&session_id, &session_metadata)
            .await?;

        let mut context_inputs =
            if let (Some(stored), Some(effective)) = (&stored_session, &effective_config) {
                let prompt_policy = self.inner_prompt_resolver()?;
                let resolved_prompt = prompt_policy
                    .resolve(
                        &agent_model_selection(&selected_model.selection),
                        stored.config_overrides.prompt_profile.as_deref(),
                        stored.config_overrides.system_prompt.as_deref(),
                    )
                    .map_err(|_| prompt_integrity_error())?;
                if effective.system_prompt_sha256 != resolved_prompt.system_prompt_sha256
                    || effective.prompt_manifest
                        != public_prompt_manifest(resolved_prompt.manifest.clone())
                {
                    return Err(prompt_integrity_error());
                }
                prompt_policy
                    .turn_context_inputs(
                        &agent_model_selection(&selected_model.selection),
                        stored.config_overrides.prompt_profile.as_deref(),
                        stored.config_overrides.system_prompt.as_deref(),
                        user_profile.as_ref(),
                    )
                    .map_err(|_| prompt_integrity_error())?
            } else {
                let mut inputs = TurnContextInputs::default();
                inputs.push_required(
                    TurnContextLayerKind::Safety,
                    TurnContextCandidate::authoritative(
                        SHARED_SAFETY_PROMPT,
                        TurnContextProvenance::new(
                            TurnContextSource::RuntimeSafety,
                            "sylvander-safety:v1",
                        ),
                    ),
                );
                if let Some(prompt) = self.system_prompt.clone()
                    && !prompt.is_empty()
                {
                    inputs.push_required(
                        TurnContextLayerKind::Agent,
                        TurnContextCandidate::authoritative(
                            prompt,
                            TurnContextProvenance::new(
                                TurnContextSource::AgentDefinition,
                                format!("agent:{}", self.id),
                            ),
                        ),
                    );
                }
                if let Some(profile) = &user_profile {
                    inputs.push_required(
                        TurnContextLayerKind::UserProfile,
                        TurnContextCandidate::authoritative(
                            profile.content(),
                            TurnContextProvenance::new(
                                TurnContextSource::UserProfile,
                                profile.provenance.source,
                            )
                            .with_revision(profile.provenance.profile_revision),
                        ),
                    );
                }
                inputs
            };

        let (agent_workspace, task_workspace, workspace_mounts) =
            effective_config
                .as_ref()
                .map_or((None, None, &[][..]), |config| {
                    (
                        config.agent_workspace.as_ref(),
                        config.user_workspace.as_ref(),
                        config.workspace_mounts.as_slice(),
                    )
                });
        let workspace = workspace_turn_context(
            agent_workspace,
            task_workspace,
            workspace_mounts,
            session_metadata.workspace.as_path(),
            &self.execution_service,
            &self.skill_features,
            &msg.payload,
            self.turn_context_budgets.workspace_knowledge,
        )
        .await?;
        if let Some(authoritative) = workspace.authoritative {
            context_inputs.push_required(TurnContextLayerKind::WorkspaceKnowledge, authoritative);
        }
        context_inputs.extend_retrieved(
            TurnContextLayerKind::WorkspaceKnowledge,
            workspace.retrieved,
        );

        if let Some(memory) = self.memory.as_ref()
            && self
                .authenticated_sessions
                .read()
                .await
                .contains(&session_id)
        {
            let execution = AgentExecutionContext::restricted_for(
                session_metadata.user_id.clone(),
                self.id.0.clone(),
                session_id.0.clone(),
            );
            let memory_context = MemoryExecutionContext::for_runtime_worker(&execution);
            let relationship = retrieve_relationship_context(
                memory.as_ref(),
                &memory_context,
                &msg.payload,
                self.turn_context_budgets.relationship_memory,
                now_secs(),
            )
            .await
            .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
            context_inputs.extend_retrieved(TurnContextLayerKind::RelationshipMemory, relationship);
        }
        if let Some(provider) = self.curated_context_provider.as_ref()
            && self
                .authenticated_sessions
                .read()
                .await
                .contains(&session_id)
        {
            let mut workspace_ids = std::collections::BTreeSet::new();
            if let Some(config) = effective_config.as_ref() {
                if let Some(binding) = config.agent_workspace.as_ref() {
                    workspace_ids.insert(binding.execution_target.clone());
                }
                if let Some(binding) = config.user_workspace.as_ref() {
                    workspace_ids.insert(binding.execution_target.clone());
                }
                workspace_ids.extend(
                    config
                        .workspace_mounts
                        .iter()
                        .map(|mount| mount.binding.execution_target.clone()),
                );
            }
            let subject = CuratedContextSubject {
                user_id: KernelUserId::new(&session_metadata.user_id),
                agent_id: KernelAgentId::new(self.id.0.clone()),
                session_id: KernelSessionId::new(session_id.0.clone()),
                workspace_ids: workspace_ids.into_iter().collect(),
            };
            let max_items = self
                .turn_context_budgets
                .workspace_knowledge
                .max_items
                .saturating_add(self.turn_context_budgets.relationship_memory.max_items)
                .min(64);
            let entries = if max_items == 0 {
                Vec::new()
            } else {
                provider
                    .retrieve(&subject, &msg.payload, max_items)
                    .await
                    .map_err(|error| AgentRunError::Configuration(error.to_string()))?
            };
            for entry in entries {
                let layer = match entry.scope {
                    CuratedMemoryScope::Relationship => TurnContextLayerKind::RelationshipMemory,
                    CuratedMemoryScope::UserProfile => TurnContextLayerKind::UserProfile,
                    CuratedMemoryScope::AgentCanonical => TurnContextLayerKind::Agent,
                    CuratedMemoryScope::WorkspaceKnowledge => {
                        TurnContextLayerKind::WorkspaceKnowledge
                    }
                };
                context_inputs.extend_retrieved(
                    layer,
                    [TurnContextCandidate::retrieved(
                        entry.content,
                        TurnContextProvenance::new(
                            TurnContextSource::GuardianCurated,
                            entry.reference,
                        )
                        .with_revision(entry.revision),
                        u32::from(entry.relevance),
                    )
                    .with_expiry(entry.expires_at_unix_secs)],
                );
            }
        }

        let composed = compose_turn_context(context_inputs, &self.turn_context_budgets, now_secs())
            .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
        let system_prompt = composed.system_prompt().to_owned();
        let context_manifest = composed.manifest;

        // 1. Persist the immutable turn boundary before provider or tool work.
        let permissions = if let Some(effective) = &effective_config {
            effective.permissions.clone()
        } else {
            self.runtime_permissions.read().await.clone()
        };
        if let (Some(store), Some(stored), Some(effective)) =
            (&self.session_store, &stored_session, &effective_config)
        {
            let user_id = match &msg.sender {
                Sender::User(user_id) => user_id.as_str(),
                _ => "unix-client",
            };
            let caller =
                sylvander_api::SessionContext::new(user_id, self.id.clone(), session_id.clone());
            let user_content = serde_json::to_value(&user_message).map_err(|_| {
                AgentRunError::session_persistence(
                    SessionPersistenceOperation::BeginTurn,
                    SessionStoreError::Invalid("user message serialization failed".into()),
                )
            })?;
            store
                .begin_turn(
                    &caller,
                    TurnStart {
                        session_id: session_id.clone(),
                        turn_id: turn_id.into(),
                        config_revision: stored.config_revision,
                        effective_config: effective.clone(),
                        user_content,
                        model_id: selected_model.shadow.reference.model.clone(),
                    },
                )
                .await
                .map_err(|source| {
                    AgentRunError::session_persistence(
                        SessionPersistenceOperation::BeginTurn,
                        source,
                    )
                })?;
            self.observability
                .record(RuntimeEvent::PersistenceFinished {
                    turn_id: turn_id.to_owned(),
                    session_id: session_id.clone(),
                    operation: RuntimePersistenceOperation::BeginTurn,
                    succeeded: true,
                });
        }
        self.turn_context_manifests
            .write()
            .await
            .insert(session_id.clone(), context_manifest);
        let history = {
            let mut sessions = self.sessions.write().await;
            let ctx = sessions
                .get_mut(&session_id)
                .ok_or_else(|| AgentRunError::UnknownSession(session_id.clone()))?;
            ctx.append_user_message(user_message);
            ctx.history_snapshot()
        };

        // 2. Build per-session approval gate and tool surface from one
        // immutable permission/capability snapshot. Changes made mid-turn
        // apply to the next turn and invalidate persistent grants there.
        let session_tool_surface = self
            .session_tool_surfaces
            .read()
            .await
            .get(&session_id)
            .cloned();
        let (turn_tools, tool_surface_revision, invocation_gateway) =
            if let Some(surface) = session_tool_surface {
                let (tools, revision) = self
                    .tools
                    .compose_session_extensions(&surface.extensions)
                    .map_err(|error| AgentRunError::Configuration(error.to_string()))?
                    .freeze_for_turn();
                let descriptors = tools.invocation_descriptors();
                let gateway = (surface.invocation_gateway_factory)(descriptors.clone())?;
                validate_tool_gateway_surface(&descriptors, gateway.as_ref())?;
                (tools, revision, gateway)
            } else {
                let (tools, revision) = self.tools.freeze_for_turn();
                (tools, revision, self.invocation_gateway.clone())
            };
        let prompt_context_features = self
            .skill_features
            .read()
            .unwrap()
            .iter()
            .map(|feature| feature.name.clone())
            .collect::<Vec<_>>();
        let invocation_snapshot = invocation_gateway
            .snapshot()
            .for_turn(&tool_surface_revision, prompt_context_features);
        let capability_revision = invocation_snapshot.revision().to_owned();
        let identity_authorized = self
            .authenticated_sessions
            .read()
            .await
            .contains(&session_id);
        let mut approval_gate: Option<Arc<dyn ApprovalGate>> = None;
        if permissions.approval_policy == sylvander_api::ApprovalPolicy::Ask {
            let grant_context = ApprovalGrantContext::new(
                session_metadata.user_id.clone(),
                self.id.clone(),
                approval_policy_revision(&permissions, &self.approval_rules),
                capability_revision,
            );
            let bus_gate: Arc<dyn ApprovalGate> = Arc::new(BusApprovalGate {
                bus: self.bus.clone(),
                agent_id: self.id.clone(),
                session_id: session_id.clone(),
                grant_context,
                persistent_identity_authorized: identity_authorized,
                pending_approvals: self.pending_approvals.clone(),
                approval_memory: self.approval_memory.clone(),
            });
            let gate: Arc<dyn ApprovalGate> = if self.approval_rules.is_empty() {
                bus_gate
            } else {
                Arc::new(sylvander_agent::approval::RuleBasedApprovalGate::new(
                    self.approval_rules.clone(),
                    bus_gate,
                ))
            };
            approval_gate = Some(gate);
        }
        if permissions.approval_policy == sylvander_api::ApprovalPolicy::Deny {
            approval_gate = Some(Arc::new(DenyAllApprovalGate));
        }
        let tool_context = tool_context_for_permissions(
            ToolSessionExecution {
                metadata: &session_metadata,
                effective_config: effective_config.as_ref(),
                execution_service: &self.execution_service,
            },
            &self.id,
            &session_id,
            &permissions,
            self.memory.is_some() && identity_authorized,
            self.workspace_journal.clone(),
            Some(turn_id),
        );
        let artifact_store = self
            .artifact_service
            .as_ref()
            .map(|service| {
                service.bind(ArtifactTurnBinding {
                    user_id: session_metadata.user_id.clone(),
                    agent_id: self.id.0.clone(),
                    session_id: session_id.0.clone(),
                    turn_id: turn_id.to_owned(),
                    created_at: tool_context.execution.started_at_unix_secs,
                })
            })
            .transpose()
            .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
        let ask_user_gate: Arc<dyn AskUserGate> = Arc::new(BusAskUserGate {
            bus: self.bus.clone(),
            agent_id: self.id.clone(),
            session_id: session_id.clone(),
            pending_answers: self.pending_answers.clone(),
        });
        let plan_gate: Arc<dyn PlanGate> = Arc::new(BusPlanGate {
            bus: self.bus.clone(),
            agent_id: self.id.clone(),
            session_id: session_id.clone(),
            pending_plans: self.pending_plans.clone(),
        });
        let reasoning = selected_effort
            .budget_tokens()
            .map(|budget_tokens| ReasoningConfig {
                budget_tokens: Some(budget_tokens.min(selected_exact_model.max_output_tokens)),
                effort: Some(match selected_effort {
                    sylvander_api::ReasoningEffort::Low => ProviderReasoningEffort::Low,
                    sylvander_api::ReasoningEffort::Medium => ProviderReasoningEffort::Medium,
                    sylvander_api::ReasoningEffort::High => ProviderReasoningEffort::High,
                    sylvander_api::ReasoningEffort::Off => {
                        unreachable!("disabled reasoning cannot carry a token budget")
                    }
                }),
            });
        let system_instructions =
            turn_system_instructions(&system_prompt, &selected_exact_model, &turn_tools);
        let selected_model_id = selected_exact_model.reference.model.clone();
        let request = AgentTurnRequest {
            conversation: ConversationSnapshot::new(history),
            model: selected_exact_model,
            system_instructions,
            reasoning,
            tools: turn_tools,
            execution: tool_context.execution.as_ref().clone(),
        };
        let mut background_request = request.clone();
        background_request.conversation = ConversationSnapshot::default();
        background_request.tools = background_request
            .tools
            .retain_named(&[ReadTool::NAME, MemoryReadTool::NAME]);
        background_request.system_instructions = turn_system_instructions(
            &system_prompt,
            &background_request.model,
            &background_request.tools,
        );
        let mut background_ports = AgentExecutionPorts::new(
            self.model_provider.clone(),
            tool_context.clone(),
            invocation_gateway.clone(),
            invocation_snapshot.clone(),
        );
        if let Some(store) = &artifact_store {
            background_ports = background_ports.with_artifact_store(store.clone());
        }
        let task_gate: Arc<dyn TaskGate> = Arc::new(BusTaskGate {
            bus: self.bus.clone(),
            agent_id: self.id.clone(),
            session_id: session_id.clone(),
            kernel: loop_config.clone(),
            request: background_request,
            ports: background_ports,
            tasks: self.background_tasks.clone(),
        });
        let mut ports = AgentExecutionPorts::new(
            self.model_provider.clone(),
            tool_context,
            invocation_gateway,
            invocation_snapshot,
        )
        .with_ask_user_gate(ask_user_gate)
        .with_plan_gate(plan_gate)
        .with_task_gate(task_gate);
        if let Some(gate) = approval_gate {
            ports = ports.with_approval_gate(gate);
        }
        if let Some(store) = artifact_store {
            ports = ports.with_artifact_store(store);
        }

        self.publish_stream(
            &session_id,
            sylvander_api::StreamEvent::TurnStarted {
                turn_id: turn_id.to_owned(),
            },
        )
        .await;

        // 3. Run loop with streaming
        let mut stream = Box::pin(agent_loop::run_stream(&loop_config, request, ports));
        tokio::pin!(interrupted);
        let mut final_message: Option<ModelResponse> = None;

        loop {
            let event = tokio::select! {
                biased;
                _ = &mut interrupted => {
                    self.cancel_pending_decisions(&session_id).await;
                    if let Some(store) = &self.session_store {
                        store
                            .finish_turn(
                                &session_id,
                                turn_id,
                                TurnState::Interrupted,
                                None,
                            )
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::FinishTurn,
                                    source,
                                )
                            })?;
                        self.observability
                            .record(RuntimeEvent::PersistenceFinished {
                                turn_id: turn_id.to_owned(),
                                session_id: session_id.clone(),
                                operation: RuntimePersistenceOperation::FinishTurn,
                                succeeded: true,
                            });
                    }
                    self.observability.record(RuntimeEvent::TurnInterrupted {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                    });
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::TurnInterrupted {
                            reason: "interrupted by user".into(),
                        },
                    ).await;
                    return Ok(());
                }
                event = stream.next() => event,
            };
            let Some(event) = event else {
                break;
            };
            match event {
                sylvander_agent::turn::event::AgentEvent::TurnTransition(transition) => {
                    self.turn_snapshots.write().await.insert(
                        session_id.clone(),
                        RuntimeTurnSnapshot {
                            turn_id: turn_id.to_owned(),
                            state: transition.into(),
                        },
                    );
                    self.observability.record(RuntimeEvent::TurnTransitioned {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                        transition,
                    });
                }
                sylvander_agent::turn::event::AgentEvent::TextChunk(text) => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::TextDelta { delta: text },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ThinkingChunk(text) => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ThinkingDelta { delta: text },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ModelRetry {
                    attempt,
                    max_attempts,
                    delay_ms,
                    reason,
                    cause,
                } => {
                    self.observability.record(RuntimeEvent::ModelRetried {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                        attempt,
                    });
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ModelRetry {
                            attempt,
                            max_attempts,
                            delay_ms,
                            reason,
                            cause: public_retry_cause(cause),
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ModelToolResponsePrepared {
                    iteration: _,
                    message,
                } => {
                    if let Some(store) = &self.session_store {
                        let caller = sylvander_api::SessionContext::new(
                            session_metadata.user_id.clone(),
                            self.id.clone(),
                            session_id.clone(),
                        )
                        .with_trace_id(turn_id);
                        let content = serde_json::to_value(message).map_err(|_| {
                            AgentRunError::session_persistence(
                                SessionPersistenceOperation::PersistModelToolResponse,
                                SessionStoreError::Invalid(
                                    "model tool response serialization failed".into(),
                                ),
                            )
                        })?;
                        store
                            .append_message(
                                &caller,
                                &session_id,
                                crate::storage::session::MessageRole::Assistant,
                                content,
                                Some(&selected_model_id),
                                None,
                                None,
                            )
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::PersistModelToolResponse,
                                    source,
                                )
                            })?;
                        self.observability
                            .record(RuntimeEvent::PersistenceFinished {
                                turn_id: turn_id.to_owned(),
                                session_id: session_id.clone(),
                                operation: RuntimePersistenceOperation::PersistModelToolResponse,
                                succeeded: true,
                            });
                    }
                }
                sylvander_agent::turn::event::AgentEvent::ToolCallPrepared {
                    id,
                    name,
                    invocation_class,
                    recovery_policy,
                    input_digest,
                    capability_revision,
                } => {
                    if let Some(store) = &self.session_store {
                        let effective_recovery_policy = if self.workspace_journal.is_some()
                            && matches!(name.as_str(), "Write" | "Edit")
                            && recovery_policy
                                == sylvander_agent::tool::invocation::ToolRecoveryPolicy::ReconcileBeforeRetry
                        {
                            recovery_policy
                        } else {
                            sylvander_agent::tool::invocation::ToolRecoveryPolicy::NeverReplay
                        };
                        store
                            .begin_tool_call(ToolCallStart {
                                session_id: session_id.clone(),
                                turn_id: turn_id.to_owned(),
                                call_id: id.clone(),
                                invocation_id: ToolInvocationId::new(),
                                tool_name: name.clone(),
                                invocation_class,
                                declared_recovery_policy: recovery_policy,
                                effective_recovery_policy,
                                capability_revision,
                                input_digest,
                            })
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::BeginToolCall,
                                    source,
                                )
                            })?;
                        self.observability
                            .record(RuntimeEvent::PersistenceFinished {
                                turn_id: turn_id.to_owned(),
                                session_id: session_id.clone(),
                                operation: RuntimePersistenceOperation::BeginToolCall,
                                succeeded: true,
                            });
                    }
                    self.observability.record(RuntimeEvent::ToolStarted {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                        tool_call_id: id,
                        tool_name: name,
                    });
                }
                sylvander_agent::turn::event::AgentEvent::ToolCallStart { id, name, input } => {
                    if let Some(store) = &self.session_store {
                        let durable = store
                            .tool_calls(&session_id, turn_id)
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::AdvanceToolCall,
                                    source,
                                )
                            })?
                            .into_iter()
                            .find(|call| call.call_id == id)
                            .ok_or_else(|| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::AdvanceToolCall,
                                    SessionStoreError::Invalid(
                                        "prepared durable tool call is missing".into(),
                                    ),
                                )
                            })?;
                        let revision = store
                            .advance_tool_call(ToolCallAdvance {
                                session_id: session_id.clone(),
                                turn_id: turn_id.to_owned(),
                                call_id: id.clone(),
                                expected_revision: durable.ledger_revision,
                                expected_position: ToolExecutionPosition::Prepared,
                                next_position: ToolExecutionPosition::Authorized,
                            })
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::AdvanceToolCall,
                                    source,
                                )
                            })?;
                        store
                            .advance_tool_call(ToolCallAdvance {
                                session_id: session_id.clone(),
                                turn_id: turn_id.to_owned(),
                                call_id: id.clone(),
                                expected_revision: revision,
                                expected_position: ToolExecutionPosition::Authorized,
                                next_position: ToolExecutionPosition::EffectStarted,
                            })
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::AdvanceToolCall,
                                    source,
                                )
                            })?;
                        self.observability
                            .record(RuntimeEvent::PersistenceFinished {
                                turn_id: turn_id.to_owned(),
                                session_id: session_id.clone(),
                                operation: RuntimePersistenceOperation::AdvanceToolCall,
                                succeeded: true,
                            });
                    }
                    if matches!(
                        name.as_str(),
                        "present_plan" | "update_plan" | "start_background_task"
                    ) {
                        continue;
                    }
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ToolCall {
                            call_id: id,
                            tool_name: name,
                            input,
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ToolCallOutputDelta {
                    id,
                    name,
                    delta,
                } => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ToolOutputDelta {
                            call_id: id,
                            tool_name: name,
                            delta,
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ToolTimedOut {
                    id,
                    name: _,
                    timeout_secs,
                } => {
                    publish_interaction_timeout(
                        &self.bus,
                        &session_id,
                        &self.id,
                        sylvander_api::InteractionTimeoutKind::Tool,
                        &id,
                        timeout_secs,
                        sylvander_api::TimeoutRecovery::NarrowScope,
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ToolCallEnd {
                    id,
                    name,
                    output,
                    is_error,
                    failure_kind,
                } => {
                    let runtime_failure_kind = match failure_kind {
                        Some(AgentToolFailureKind::FilesystemBoundaryPolicyViolation) => {
                            Some(RuntimeToolFailureKind::FilesystemBoundaryPolicyViolation)
                        }
                        Some(AgentToolFailureKind::Unclassified) | None => None,
                    };
                    if let Some(store) = &self.session_store {
                        let stored_failure_kind = match failure_kind {
                            Some(AgentToolFailureKind::FilesystemBoundaryPolicyViolation) => {
                                Some(StoredToolCallFailureKind::FilesystemBoundaryPolicyViolation)
                            }
                            Some(AgentToolFailureKind::Unclassified) | None => None,
                        };
                        let durable = store
                            .tool_calls(&session_id, turn_id)
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::PersistToolResult,
                                    source,
                                )
                            })?
                            .into_iter()
                            .find(|call| call.call_id == id)
                            .ok_or_else(|| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::PersistToolResult,
                                    SessionStoreError::Invalid(
                                        "running durable tool call is missing".into(),
                                    ),
                                )
                            })?;
                        let mut ledger_revision = durable.ledger_revision;
                        let mut result_position = ToolExecutionPosition::EffectStarted;
                        if !is_error {
                            ledger_revision = store
                                .advance_tool_call(ToolCallAdvance {
                                    session_id: session_id.clone(),
                                    turn_id: turn_id.to_owned(),
                                    call_id: id.clone(),
                                    expected_revision: ledger_revision,
                                    expected_position: ToolExecutionPosition::EffectStarted,
                                    next_position: ToolExecutionPosition::EffectCommitted,
                                })
                                .await
                                .map_err(|source| {
                                    AgentRunError::session_persistence(
                                        SessionPersistenceOperation::AdvanceToolCall,
                                        source,
                                    )
                                })?;
                            result_position = ToolExecutionPosition::EffectCommitted;
                        }
                        let result_content = serde_json::to_value(ChatMessage::user_blocks(vec![
                            ContentBlock::tool_result_text(id.clone(), output.clone(), is_error),
                        ]))
                        .map_err(|_| {
                            AgentRunError::session_persistence(
                                SessionPersistenceOperation::PersistToolResult,
                                SessionStoreError::Invalid(
                                    "tool result serialization failed".into(),
                                ),
                            )
                        })?;
                        let caller = sylvander_api::SessionContext::new(
                            session_metadata.user_id.clone(),
                            self.id.clone(),
                            session_id.clone(),
                        )
                        .with_trace_id(turn_id);
                        store
                            .persist_tool_result(
                                &caller,
                                ToolResultPersistence {
                                    session_id: session_id.clone(),
                                    turn_id: turn_id.to_owned(),
                                    call_id: id.clone(),
                                    expected_revision: ledger_revision,
                                    expected_position: result_position,
                                    content: result_content,
                                    tool_name: name.clone(),
                                    terminal_state: if is_error {
                                        ToolCallState::Failed
                                    } else {
                                        ToolCallState::Succeeded
                                    },
                                    failure_kind: stored_failure_kind,
                                },
                            )
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::PersistToolResult,
                                    source,
                                )
                            })?;
                        self.observability
                            .record(RuntimeEvent::PersistenceFinished {
                                turn_id: turn_id.to_owned(),
                                session_id: session_id.clone(),
                                operation: RuntimePersistenceOperation::PersistToolResult,
                                succeeded: true,
                            });
                    }
                    self.observability.record(RuntimeEvent::ToolFinished {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                        tool_call_id: id.clone(),
                        tool_name: name.clone(),
                        succeeded: !is_error,
                        failure_kind: runtime_failure_kind,
                    });
                    if matches!(
                        name.as_str(),
                        "present_plan" | "update_plan" | "start_background_task"
                    ) {
                        continue;
                    }
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ToolResult {
                            call_id: id,
                            tool_name: name,
                            output,
                            is_error,
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::ToolRejected { id, name, reason } => {
                    if let Some(store) = &self.session_store {
                        store
                            .finish_tool_call(ToolCallCompletion {
                                session_id: session_id.clone(),
                                turn_id: turn_id.to_owned(),
                                call_id: id.clone(),
                                state: ToolCallState::Rejected,
                                failure_kind: None,
                            })
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::FinishToolCall,
                                    source,
                                )
                            })?;
                        self.observability
                            .record(RuntimeEvent::PersistenceFinished {
                                turn_id: turn_id.to_owned(),
                                session_id: session_id.clone(),
                                operation: RuntimePersistenceOperation::FinishToolCall,
                                succeeded: true,
                            });
                    }
                    self.observability.record(RuntimeEvent::ToolFinished {
                        turn_id: turn_id.to_owned(),
                        session_id: session_id.clone(),
                        tool_call_id: id.clone(),
                        tool_name: name.clone(),
                        succeeded: false,
                        failure_kind: None,
                    });
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::ToolResult {
                            call_id: id,
                            tool_name: name,
                            output: reason,
                            is_error: true,
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::IterationStart { iteration } => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::IterationStart { iteration },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::IterationEnd {
                    iteration,
                    usage,
                    provider_usage,
                } => {
                    self.context_usage.write().await.insert(
                        session_id.clone(),
                        ContextUsage {
                            used: u32::try_from(provider_usage.total_input_tokens())
                                .unwrap_or(u32::MAX),
                            cache_read: u32::try_from(
                                provider_usage.cache_read_tokens.unwrap_or(0),
                            )
                            .unwrap_or(u32::MAX),
                            cache_write: u32::try_from(
                                provider_usage.cache_write_tokens.unwrap_or(0),
                            )
                            .unwrap_or(u32::MAX),
                        },
                    );
                    let mut input_tokens = usage.input_tokens;
                    let mut output_tokens = usage.output_tokens;
                    let iteration_cost = selected_pricing
                        .and_then(|pricing| usage_cost_nano_usd(pricing, &provider_usage));
                    let mut cost_nano_usd = iteration_cost;
                    if let Some(store) = &self.session_store {
                        let total = store
                            .record_usage(
                                &session_id,
                                u32::try_from(provider_usage.input_tokens).unwrap_or(u32::MAX),
                                u32::try_from(provider_usage.output_tokens).unwrap_or(u32::MAX),
                                iteration_cost,
                            )
                            .await
                            .map_err(|source| {
                                AgentRunError::session_persistence(
                                    SessionPersistenceOperation::RecordUsage,
                                    source,
                                )
                            })?;
                        input_tokens = total.input_tokens;
                        output_tokens = total.output_tokens;
                        cost_nano_usd = total.cost_nano_usd;
                        self.observability
                            .record(RuntimeEvent::PersistenceFinished {
                                turn_id: turn_id.to_owned(),
                                session_id: session_id.clone(),
                                operation: RuntimePersistenceOperation::RecordUsage,
                                succeeded: true,
                            });
                    }
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::IterationEnd {
                            iteration,
                            input_tokens: u32::try_from(input_tokens).unwrap_or(u32::MAX),
                            output_tokens: u32::try_from(output_tokens).unwrap_or(u32::MAX),
                            cost_nano_usd,
                        },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::CompressionStarted => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::CompactionStarted { automatic: true },
                    )
                    .await;
                }
                // `BusAskUserGate` publishes the request when it installs the
                // pending answer. Forwarding the loop event too would stack
                // two identical TUI modals for one question.
                sylvander_agent::turn::event::AgentEvent::Compressed { .. }
                | sylvander_agent::turn::event::AgentEvent::AskUser { .. }
                | sylvander_agent::turn::event::AgentEvent::PlanProposed { .. }
                | sylvander_agent::turn::event::AgentEvent::PlanResolved { .. } => {}
                sylvander_agent::turn::event::AgentEvent::HistoryCompacted { layers, history } => {
                    if let Some(error) =
                        sylvander_agent::compress::layer::first_failure_error(&layers)
                    {
                        self.publish_stream(
                            &session_id,
                            sylvander_api::StreamEvent::CompactionFailed {
                                automatic: true,
                                reason: error.compatibility_reason().into(),
                            },
                        )
                        .await;
                    } else {
                        match self
                            .apply_compacted_history(&session_id, &history, &layers)
                            .await
                        {
                            Ok(()) => {
                                self.publish_stream(
                                    &session_id,
                                    sylvander_api::StreamEvent::CompactionCompleted {
                                        report: public_compaction_report(true, &layers),
                                    },
                                )
                                .await;
                            }
                            Err(error) => {
                                self.publish_stream(
                                    &session_id,
                                    sylvander_api::StreamEvent::CompactionFailed {
                                        automatic: true,
                                        reason: sylvander_agent::compress::error::CompactionError::new(
                                            sylvander_agent::compress::error::CompactionFailureCode::Persistence,
                                        )
                                        .compatibility_reason()
                                        .into(),
                                    },
                                )
                                .await;
                                return Err(error);
                            }
                        }
                    }
                }
                sylvander_agent::turn::event::AgentEvent::UserAnswer { call_id, answer } => {
                    self.publish_stream(
                        &session_id,
                        sylvander_api::StreamEvent::UserAnswer { call_id, answer },
                    )
                    .await;
                }
                sylvander_agent::turn::event::AgentEvent::Done(outcome) => {
                    final_message = Some(outcome.final_response);
                }
                sylvander_agent::turn::event::AgentEvent::Error(e) => {
                    return Err(AgentRunError::Loop(e));
                }
            }
        }

        // 4. Write final message, record the terminal fact, then publish Done.
        let msg = final_message.ok_or_else(|| {
            AgentRunError::Loop(AgentLoopError::Validation(
                "Agent event stream ended without a terminal outcome".into(),
            ))
        })?;
        let text = msg.text();
        if let Some(store) = &self.session_store {
            let user_id = self.sessions.read().await.get(&session_id).map_or_else(
                || "unix-client".into(),
                |context| context.metadata.user_id.clone(),
            );
            let caller =
                sylvander_api::SessionContext::new(user_id, self.id.clone(), session_id.clone());
            let message = ChatMessage::assistant(msg.content.clone());
            let content = serde_json::to_value(message).map_err(|_| {
                AgentRunError::session_persistence(
                    SessionPersistenceOperation::CompleteTurn,
                    SessionStoreError::Invalid("assistant message serialization failed".into()),
                )
            })?;
            store
                .complete_turn(
                    &caller,
                    TurnCompletion {
                        session_id: session_id.clone(),
                        turn_id: turn_id.to_owned(),
                        assistant_content: content,
                        model_id: msg.model.model.clone(),
                    },
                )
                .await
                .map_err(|source| {
                    AgentRunError::session_persistence(
                        SessionPersistenceOperation::CompleteTurn,
                        source,
                    )
                })?;
            self.observability
                .record(RuntimeEvent::PersistenceFinished {
                    turn_id: turn_id.to_owned(),
                    session_id: session_id.clone(),
                    operation: RuntimePersistenceOperation::CompleteTurn,
                    succeeded: true,
                });
        }
        let mut sessions = self.sessions.write().await;
        if let Some(ctx) = sessions.get_mut(&session_id) {
            ctx.append_assistant_message(msg);
        }
        drop(sessions);
        self.observability.record(RuntimeEvent::TurnCompleted {
            turn_id: turn_id.to_owned(),
            session_id: session_id.clone(),
        });
        self.publish_stream(&session_id, sylvander_api::StreamEvent::Done { text })
            .await;

        Ok(())
    }

    // -- helpers --

    async fn publish_stream(&self, session_id: &SessionId, event: sylvander_api::StreamEvent) {
        let msg = BusMessage::stream_event(session_id.clone(), self.id.clone(), event);
        let _ = self.bus.publish(msg).await;
    }

    async fn publish_error(&self, session_id: &SessionId, err: &AgentLoopError) {
        self.publish_stream(
            session_id,
            sylvander_api::StreamEvent::Error {
                message: err.to_string(),
            },
        )
        .await;
    }

    pub(super) fn message_to_param(msg: &BusMessage) -> ChatMessage {
        if msg.attachments.is_empty() {
            return ChatMessage::user(&msg.payload);
        }
        let mut blocks = Vec::new();
        if !msg.payload.is_empty() {
            blocks.push(ContentBlock::Text {
                text: msg.payload.clone(),
            });
        }
        for attachment in &msg.attachments {
            match &attachment.content {
                sylvander_api::AttachmentContent::Text { text } => {
                    blocks.push(ContentBlock::Text {
                        text: format!(
                            "Attached {:?} `{}` ({}):\n{}",
                            attachment.kind, attachment.name, attachment.mime_type, text
                        ),
                    });
                }
                sylvander_api::AttachmentContent::Base64 { data } => {
                    if matches!(attachment.mime_type.as_str(), "image/png" | "image/jpeg") {
                        blocks.push(ContentBlock::Text {
                            text: format!("Attached image `{}`:", attachment.name),
                        });
                        blocks.push(ContentBlock::Image {
                            image: ImageContent {
                                source: MediaSource::Base64 {
                                    media_type: attachment.mime_type.clone(),
                                    data: data.clone(),
                                },
                                alt_text: Some(attachment.name.clone()),
                            },
                        });
                    }
                }
            }
        }
        ChatMessage::user_blocks(blocks)
    }
}

pub(super) struct WorkspaceTurnContext {
    pub(super) authoritative: Option<TurnContextCandidate>,
    retrieved: Vec<TurnContextCandidate>,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn workspace_turn_context(
    agent_workspace: Option<&sylvander_api::SessionWorkspaceBinding>,
    task_workspace: Option<&sylvander_api::SessionWorkspaceBinding>,
    workspace_mounts: &[sylvander_api::SessionWorkspaceMount],
    fallback_task_workspace: &Path,
    execution_service: &RuntimeExecutionService,
    skill_features: &std::sync::RwLock<Vec<sylvander_api::PlatformFeature>>,
    query: &str,
    budget: sylvander_agent::turn_context::TurnContextBudget,
) -> Result<WorkspaceTurnContext, AgentRunError> {
    let agent_focus = agent_workspace
        .and_then(|binding| binding.instruction_focus.clone())
        .unwrap_or_default();
    let task_focus = task_workspace
        .and_then(|binding| binding.instruction_focus.clone())
        .unwrap_or_default();
    let agent_target = agent_workspace.map(workspace_target);
    let task_target = Some(task_workspace.map_or_else(
        || WorkspaceTarget::local(fallback_task_workspace, true),
        |binding| WorkspaceTarget {
            id: binding.execution_target.clone(),
            workspace_path: if binding.execution_target == "local" {
                fallback_task_workspace.to_path_buf()
            } else {
                binding.path.clone()
            },
            read_only: true,
        },
    ));
    let agent_executor = agent_target
        .as_ref()
        .map(|target| workspace_context_executor(execution_service, target))
        .transpose()?;
    let task_executor = task_target
        .as_ref()
        .map(|target| workspace_context_executor(execution_service, target))
        .transpose()?;
    let context = workspace_context::discover_with_report(
        agent_target
            .clone()
            .zip(agent_executor)
            .map(|(target, executor)| {
                workspace_context::WorkspaceContextSource::focused(
                    executor.as_ref(),
                    target,
                    agent_focus,
                )
            }),
        task_target
            .clone()
            .zip(task_executor)
            .map(|(target, executor)| {
                workspace_context::WorkspaceContextSource::focused(
                    executor.as_ref(),
                    target,
                    task_focus,
                )
            }),
    )
    .await
    .map_err(|error| AgentRunError::Configuration(error.to_string()))?;
    *skill_features.write().unwrap() = context
        .skills
        .iter()
        .map(|skill| sylvander_api::PlatformFeature {
            kind: sylvander_api::PlatformFeatureKind::Skill,
            name: skill.name.clone(),
            status: match skill.status {
                workspace_context::SkillStatus::Active => {
                    sylvander_api::PlatformFeatureStatus::Active
                }
                workspace_context::SkillStatus::Disabled => {
                    sylvander_api::PlatformFeatureStatus::Configured
                }
                workspace_context::SkillStatus::Degraded => {
                    sylvander_api::PlatformFeatureStatus::Degraded
                }
            },
            summary: format!("prompt context only · {} ({})", skill.summary, skill.role),
            source: Some(format!("{}:{}", skill.target_id, skill.relative_path)),
            trust: Some(if skill.role == "agent-home" {
                sylvander_api::PlatformTrust::BuiltIn
            } else {
                sylvander_api::PlatformTrust::Workspace
            }),
            auth: sylvander_api::PlatformAuthStatus::NotRequired,
            capabilities: {
                let mut capabilities = skill.capabilities.clone();
                capabilities.push("prompt_context_only".into());
                capabilities
            },
            reloadable: true,
        })
        .collect();
    let mut prompt = context.prompt.unwrap_or_default();
    if !workspace_mounts.is_empty() {
        let mounts = workspace_mounts
            .iter()
            .map(|mount| {
                let mut operations = vec!["read"];
                if mount.capabilities.write {
                    operations.push("write");
                }
                if mount.capabilities.command {
                    operations.push("command");
                }
                if mount.capabilities.git {
                    operations.push("git");
                }
                let role = match mount.role {
                    sylvander_api::WorkspaceMountRole::AgentHome => "agent-home",
                    sylvander_api::WorkspaceMountRole::Task => "task",
                    sylvander_api::WorkspaceMountRole::Dependency => "dependency",
                    sylvander_api::WorkspaceMountRole::Artifact => "artifact",
                };
                format!("- @{} ({role}): {}", mount.reference, operations.join(", "))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !prompt.is_empty() {
            prompt.push_str("\n\n");
        }
        prompt.push_str(
            "# Available workspace mounts\n\
             Unqualified paths use the task workspace. Address another mount \
             as `@reference/path`; Command and Git accept `workspace: \"reference\"`.\n",
        );
        prompt.push_str(&mounts);
    }
    let authoritative = (!prompt.is_empty()).then(|| {
        TurnContextCandidate::authoritative(
            prompt,
            TurnContextProvenance::new(
                TurnContextSource::WorkspaceInstructions,
                task_target.as_ref().map_or_else(
                    || "workspace:unavailable".into(),
                    |target| format!("workspace:{}:instructions", target.id),
                ),
            ),
        )
    });
    let retrieved = match (task_target.as_ref(), task_executor) {
        (Some(target), Some(executor)) => {
            retrieve_workspace_context(executor.as_ref(), target, query, budget)
                .await
                .map_err(|error| AgentRunError::Configuration(error.to_string()))?
        }
        _ => Vec::new(),
    };
    Ok(WorkspaceTurnContext {
        authoritative,
        retrieved,
    })
}

fn workspace_target(binding: &sylvander_api::SessionWorkspaceBinding) -> WorkspaceTarget {
    WorkspaceTarget {
        id: binding.execution_target.clone(),
        workspace_path: binding.path.clone(),
        read_only: true,
    }
}

fn workspace_context_executor<'a>(
    execution_service: &'a RuntimeExecutionService,
    target: &WorkspaceTarget,
) -> Result<&'a Arc<dyn WorkspaceExecutor>, AgentRunError> {
    execution_service.resolve(&target.id).ok_or_else(|| {
        AgentRunError::Configuration(format!(
            "execution target `{}` is unavailable on this server",
            target.id
        ))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TurnCorrelation {
    pub(super) turn: String,
    pub(super) request: String,
    pub(super) trace: String,
}

impl TurnCorrelation {
    pub(super) fn new(message: &BusMessage, turn_id: uuid::Uuid) -> Self {
        let turn_id = turn_id.to_string();
        Self {
            request: message.id.0.to_string(),
            trace: turn_id.clone(),
            turn: turn_id,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ToolSessionExecution<'a> {
    pub(super) metadata: &'a SessionMetadata,
    pub(super) effective_config: Option<&'a sylvander_api::SessionEffectiveConfig>,
    pub(super) execution_service: &'a RuntimeExecutionService,
}

pub(super) fn tool_context_for_permissions(
    execution: ToolSessionExecution<'_>,
    agent_id: &AgentId,
    session_id: &SessionId,
    permissions: &sylvander_api::PermissionProfile,
    trusted_memory: bool,
    workspace_journal: Option<Arc<WorkspaceJournal>>,
    turn_id: Option<&str>,
) -> ToolContext {
    let metadata = execution.metadata;
    let binding = execution.effective_config.and_then(|config| {
        select_workspace_binding(
            config.user_workspace.as_ref(),
            config.agent_workspace.as_ref(),
        )
    });
    let target_id = binding.map_or("local", |binding| binding.execution_target.as_str());
    let workspace = binding.map_or(metadata.workspace.as_path(), |binding| {
        binding.path.as_path()
    });
    let permission_read_only = permissions.file_access != sylvander_api::FileAccess::WorkspaceWrite;
    let read_only = permission_read_only || binding.is_some_and(|binding| binding.read_only);
    let mut agent_execution = AgentExecutionContext::restricted_for(
        metadata.user_id.clone(),
        agent_id.0.clone(),
        session_id.0.clone(),
    )
    .with_started_at_unix_secs(now_secs())
    .with_workspace(ExecutionWorkspace {
        workspace_id: "primary".into(),
        target_id: target_id.to_owned(),
        read_only,
    });
    if let Some(turn_id) = turn_id {
        agent_execution = agent_execution.with_turn_id(turn_id).with_trace_id(turn_id);
    }
    let mut context = if trusted_memory {
        ToolContext::for_runtime(agent_execution)
    } else {
        ToolContext::new(agent_execution)
    };
    let target = WorkspaceTarget {
        id: target_id.to_owned(),
        workspace_path: workspace.to_path_buf(),
        read_only,
    };
    let executor = execution
        .effective_config
        .filter(|config| !config.workspace_mounts.is_empty())
        .map_or_else(
            || {
                execution
                    .execution_service
                    .resolve_or_unavailable(target_id)
            },
            |config| {
                let default_reference = config
                    .workspace_mounts
                    .iter()
                    .find(|mount| mount.role == sylvander_api::WorkspaceMountRole::Task)
                    .or_else(|| config.workspace_mounts.first())
                    .map(|mount| mount.reference.clone())
                    .expect("non-empty workspace composition has a default mount");
                let mounts = config.workspace_mounts.iter().map(|mount| {
                    let mut capabilities = WorkspaceCapabilities {
                        read: mount.capabilities.read,
                        write: mount.capabilities.write,
                        command: mount.capabilities.command,
                        git: mount.capabilities.git,
                    };
                    if permission_read_only {
                        capabilities.write = false;
                        capabilities.command = false;
                    }
                    let executor = execution
                        .execution_service
                        .resolve_or_unavailable(&mount.binding.execution_target);
                    (
                        mount.reference.clone(),
                        MountedWorkspace {
                            executor,
                            target: WorkspaceTarget {
                                id: mount.binding.execution_target.clone(),
                                workspace_path: mount.binding.path.clone(),
                                read_only: permission_read_only || mount.binding.read_only,
                            },
                            capabilities,
                        },
                    )
                });
                WorkspaceRouter::new(default_reference, mounts).map_or_else(
                    |_| {
                        Arc::new(UnavailableExecutor::new("workspace-composition"))
                            as Arc<dyn WorkspaceExecutor>
                    },
                    |router| Arc::new(router) as Arc<dyn WorkspaceExecutor>,
                )
            },
        );
    context = context.with_executor(executor, target);
    if target_id == "local"
        && !read_only
        && let Some(journal) = workspace_journal
    {
        let journal: Arc<dyn WorkspaceMutationJournal> = journal;
        context = context.with_workspace_journal(journal);
    }
    match permissions.file_access {
        sylvander_api::FileAccess::None => {}
        sylvander_api::FileAccess::ReadOnly => {
            context = context.with_capability(Cap::Read).with_capability(Cap::Git);
        }
        sylvander_api::FileAccess::WorkspaceWrite => {
            context = context
                .with_capability(Cap::Read)
                .with_capability(Cap::Write)
                .with_capability(Cap::Spawn)
                .with_capability(Cap::Git);
        }
    }
    if permissions.network_access == sylvander_api::NetworkAccess::Allowed {
        context = context.with_capability(Cap::Network);
        context.surface.network = NetworkPolicy::All;
    }
    if trusted_memory {
        context = context
            .with_capability(Cap::MemoryRead)
            .with_capability(Cap::MemoryWrite);
    }
    context
}

pub(super) fn select_workspace_binding<'a>(
    user_workspace: Option<&'a sylvander_api::SessionWorkspaceBinding>,
    agent_workspace: Option<&'a sylvander_api::SessionWorkspaceBinding>,
) -> Option<&'a sylvander_api::SessionWorkspaceBinding> {
    user_workspace.or(agent_workspace)
}
