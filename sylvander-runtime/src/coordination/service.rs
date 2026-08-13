//! Governed application service for durable inter-Agent coordination.

mod arbitration;
mod define;
mod handoff;
mod message;
mod relation;
mod spawn;
mod task;

use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};
use sylvander_api::HandoffId;
use sylvander_api::{AgentInstanceId, CoordinationMessageId, GovernanceCaseId, SessionId, TaskId};

use crate::agent::instance::{
    AgentDefinitionKey, AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute,
    HistoryView, SessionAgentRole,
};
use crate::coordination::arbitration::ModeratorVerdict;
use crate::coordination::arbitration::{ArbitrationCase, ArbitrationState, ModeratorDecision};
use crate::coordination::governance::{
    GovernanceAssessment, GovernanceFinding, GovernancePolicy, GovernanceSnapshot,
    ProgressObservation, WaitDependency, assess,
};
use crate::coordination::handoff::{HandoffState, TaskHandoff};
use crate::coordination::mailbox::{
    AgentMessageTurn, CoordinationMessage, CoordinationMessageKind, MessageClaim,
    MessageDeliveryState,
};
use crate::coordination::task::SessionTaskGraph;
use crate::coordination::task::{CoordinationTask, CoordinationTaskState};
use crate::coordination::topology::{AgentRelation, AgentRelationKind, SessionTopology};
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::SessionStoreError;

pub const DEFAULT_ARBITRATION_TTL_SECONDS: u64 = 300;
const MAX_ARBITRATION_RENEWALS: usize = 64;

/// Stable caller intent. Runtime derives every governance fact and route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DispatchMessageRequest {
    pub message_id: CoordinationMessageId,
    pub session_id: SessionId,
    pub sender_instance_id: AgentInstanceId,
    pub recipient_instance_id: AgentInstanceId,
    pub task_id: Option<TaskId>,
    pub kind: CoordinationMessageKind,
    pub payload: String,
    pub max_hops: u16,
    pub expires_at: i64,
}

/// A dispatch either became durable or was durably fenced for moderation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchMessageOutcome {
    Enqueued(CoordinationMessage),
    EnqueuedByModerator {
        message: CoordinationMessage,
        decision: ModeratorDecision,
    },
    RequiresArbitration {
        case: ArbitrationCase,
        assessment: GovernanceAssessment,
    },
    RejectedByModerator {
        case: ArbitrationCase,
        decision: ModeratorDecision,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForkAgentRequest {
    pub instance_id: AgentInstanceId,
    pub session_id: SessionId,
    pub parent_instance_id: AgentInstanceId,
    pub branch_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkAgentOutcome {
    Created(AgentInstance),
    CreatedByModerator {
        participant: AgentInstance,
        decision: ModeratorDecision,
    },
    RequiresArbitration {
        case: ArbitrationCase,
        assessment: GovernanceAssessment,
    },
    RejectedByModerator {
        case: ArbitrationCase,
        decision: ModeratorDecision,
    },
}

/// Runtime-resolved intent to add a separately defined Agent to one Session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DefineAgentRequest {
    pub instance_id: AgentInstanceId,
    pub session_id: SessionId,
    pub sponsor_instance_id: AgentInstanceId,
    pub definition: AgentDefinitionKey,
    pub role: SessionAgentRole,
    pub capability_revision: String,
    pub effective_config: sylvander_api::SessionEffectiveConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefineAgentOutcome {
    Created(AgentInstance),
    CreatedByModerator {
        participant: AgentInstance,
        decision: ModeratorDecision,
    },
    RequiresArbitration {
        case: ArbitrationCase,
        assessment: GovernanceAssessment,
    },
    RejectedByModerator {
        case: ArbitrationCase,
        decision: ModeratorDecision,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelateAgentsRequest {
    pub session_id: SessionId,
    pub requested_by: AgentInstanceId,
    pub source: AgentInstanceId,
    pub target: AgentInstanceId,
    pub kind: AgentRelationKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelateAgentsOutcome {
    Applied(SessionTopology),
    AppliedByModerator {
        topology: SessionTopology,
        decision: ModeratorDecision,
    },
    RequiresArbitration {
        case: ArbitrationCase,
        assessment: GovernanceAssessment,
    },
    RejectedByModerator {
        case: ArbitrationCase,
        decision: ModeratorDecision,
    },
}

/// Stable handoff intent; Runtime derives revisions and the correct arbitrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposeHandoffRequest {
    pub handoff_id: HandoffId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub from_instance_id: AgentInstanceId,
    pub to_instance_id: AgentInstanceId,
    pub requested_by: AgentInstanceId,
    pub reason: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportWaitRequest {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub waiter: AgentInstanceId,
    pub awaited: AgentInstanceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportProgressRequest {
    pub observation_id: String,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub agent_instance_id: AgentInstanceId,
    pub consumed_tokens: u64,
    pub evidence_digest: Option<String>,
}

/// Stable Agent-authored intent to add one bounded unit to the Session DAG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTaskRequest {
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub parent_task_id: Option<TaskId>,
    pub created_by: AgentInstanceId,
    pub assigned_to: AgentInstanceId,
    pub objective: String,
    pub token_budget: u64,
    pub max_handoffs: u32,
}

/// Agent-authored lifecycle update. Runtime derives the revision fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionTaskRequest {
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub actor: AgentInstanceId,
    pub next_state: CoordinationTaskState,
    pub consumed_tokens: u64,
}

/// Single policy-enforcing entry point above coordination repositories.
pub struct CoordinationService<S> {
    store: Arc<S>,
    policy: GovernancePolicy,
    arbitration_ttl_seconds: u64,
}

impl<S> CoordinationService<S>
where
    S: AgentInstanceStore + CoordinationStore,
{
    #[must_use]
    pub fn new(store: Arc<S>, policy: GovernancePolicy, arbitration_ttl_seconds: u64) -> Self {
        Self {
            store,
            policy,
            arbitration_ttl_seconds,
        }
    }

    /// Add or refresh one wait-for edge using current durable revision fences.
    pub async fn report_wait(
        &self,
        request: &ReportWaitRequest,
        now: i64,
    ) -> Result<(), CoordinationServiceError> {
        let membership = self
            .store
            .session_membership(&request.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.session_id.clone())
            })?;
        ensure_available(&membership, &request.waiter)?;
        ensure_available(&membership, &request.awaited)?;
        let topology = self
            .store
            .topology(&request.session_id)
            .await?
            .ok_or_else(|| CoordinationServiceError::MissingTopology(request.session_id.clone()))?;
        let task = self
            .store
            .task(&request.task_id)
            .await?
            .ok_or(CoordinationServiceError::UnknownTask)?;
        self.store
            .record_wait(
                &request.session_id,
                &WaitDependency {
                    task_id: request.task_id.clone(),
                    waiter: request.waiter.clone(),
                    awaited: request.awaited.clone(),
                },
                task.revision,
                topology.topology_revision,
                now,
            )
            .await
            .map_err(Into::into)
    }

    /// Resolve one exact wait edge. Repetition is intentionally idempotent.
    pub async fn clear_wait(
        &self,
        request: &ReportWaitRequest,
    ) -> Result<(), CoordinationServiceError> {
        self.store
            .clear_wait(
                &request.session_id,
                &WaitDependency {
                    task_id: request.task_id.clone(),
                    waiter: request.waiter.clone(),
                    awaited: request.awaited.clone(),
                },
            )
            .await
            .map_err(Into::into)
    }

    /// Append an idempotent progress sample bound to current task execution.
    pub async fn report_progress(
        &self,
        request: ReportProgressRequest,
        now: i64,
    ) -> Result<ProgressObservation, CoordinationServiceError> {
        let membership = self
            .store
            .session_membership(&request.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.session_id.clone())
            })?;
        ensure_available(&membership, &request.agent_instance_id)?;
        let task = self
            .store
            .task(&request.task_id)
            .await?
            .ok_or(CoordinationServiceError::UnknownTask)?;
        let observation = ProgressObservation {
            observation_id: request.observation_id,
            task_id: request.task_id,
            agent_instance_id: request.agent_instance_id,
            task_revision: task.revision,
            consumed_tokens: request.consumed_tokens,
            evidence_digest: request.evidence_digest,
            observed_at: now,
        };
        self.store
            .record_progress(&request.session_id, &observation)
            .await?;
        Ok(observation)
    }

    async fn ensure_arbitration_notification(
        &self,
        sender_instance_id: &AgentInstanceId,
        task_id: Option<&TaskId>,
        case: &ArbitrationCase,
        membership: &crate::session::membership::SessionMembership,
        topology: &crate::coordination::topology::SessionTopology,
    ) -> Result<(), CoordinationServiceError> {
        let route = topology
            .route_between(sender_instance_id, &case.moderator_instance_id)
            .ok_or(CoordinationServiceError::Unroutable)?;
        let hops = u16::try_from(route.len().saturating_sub(1))
            .map_err(|_| CoordinationServiceError::InvalidConfiguration)?;
        let message = CoordinationMessage {
            message_id: CoordinationMessageId::new(format!("arbitration:{}", case.case_id.0)),
            session_id: case.session_id.clone(),
            sender_instance_id: sender_instance_id.clone(),
            recipient_instance_id: case.moderator_instance_id.clone(),
            task_id: task_id.cloned(),
            kind: CoordinationMessageKind::Control,
            payload: format!("governance_case:{}", case.case_id.0),
            topology_revision: topology.topology_revision,
            route,
            max_hops: hops.max(1),
            state: MessageDeliveryState::Pending,
            delivery_attempts: 0,
            revision: 0,
            expires_at: case.expires_at,
            created_at: case.created_at,
            updated_at: case.created_at,
        };
        if let Some(existing) = self.store.message(&message.message_id).await? {
            return if same_dispatch_intent(&existing, &message) {
                Ok(())
            } else {
                Err(CoordinationServiceError::IdempotencyConflict)
            };
        }
        self.store
            .enqueue_message(&message, membership, topology, case.created_at)
            .await
            .map_err(Into::into)
    }
}

fn governance_case_id(
    prefix: &str,
    request: &impl Serialize,
    membership_revision: u64,
    topology_revision: u64,
) -> Result<GovernanceCaseId, CoordinationServiceError> {
    let encoded = serde_json::to_vec(request)
        .map_err(|error| CoordinationServiceError::InvalidDispatch(error.to_string()))?;
    let mut digest = Sha256::new();
    digest.update(encoded);
    digest.update(membership_revision.to_be_bytes());
    digest.update(topology_revision.to_be_bytes());
    Ok(GovernanceCaseId::new(format!(
        "{prefix}:{:x}",
        digest.finalize()
    )))
}

fn ensure_available(
    membership: &crate::session::membership::SessionMembership,
    instance_id: &AgentInstanceId,
) -> Result<(), CoordinationServiceError> {
    let participant = membership
        .participants
        .iter()
        .find(|participant| &participant.instance_id == instance_id)
        .ok_or(CoordinationServiceError::UnknownAgent)?;
    if participant.state.is_terminal()
        || participant.state == AgentInstanceState::ManualReconciliation
    {
        return Err(CoordinationServiceError::UnavailableAgent);
    }
    Ok(())
}

fn same_dispatch_intent(existing: &CoordinationMessage, proposed: &CoordinationMessage) -> bool {
    existing.message_id == proposed.message_id
        && existing.session_id == proposed.session_id
        && existing.sender_instance_id == proposed.sender_instance_id
        && existing.recipient_instance_id == proposed.recipient_instance_id
        && existing.task_id == proposed.task_id
        && existing.kind == proposed.kind
        && existing.payload == proposed.payload
        && existing.topology_revision == proposed.topology_revision
        && existing.route == proposed.route
        && existing.max_hops == proposed.max_hops
        && existing.expires_at == proposed.expires_at
}

#[derive(Debug, thiserror::Error)]
pub enum CoordinationServiceError {
    #[error("Session {0} has no durable Agent membership")]
    MissingMembership(SessionId),
    #[error("Session {0} has no durable Agent topology")]
    MissingTopology(SessionId),
    #[error("coordination references an unknown Agent instance")]
    UnknownAgent,
    #[error("coordination targets an unavailable Agent instance")]
    UnavailableAgent,
    #[error("coordination references an unknown task")]
    UnknownTask,
    #[error("coordination task is invalid: {0}")]
    InvalidTask(String),
    #[error("coordination task is blocked by a hard governance finding")]
    GovernanceBlocked,
    #[error("coordination references an unknown handoff")]
    UnknownHandoff,
    #[error("coordination references an unknown arbitration case")]
    UnknownArbitration,
    #[error("handoff decision was not issued by its governed arbitrator")]
    UnauthorizedArbitrator,
    #[error("Agent actor is not authorized for this coordination intent")]
    UnauthorizedActor,
    #[error("recipient is not reachable in the governed topology")]
    Unroutable,
    #[error("coordination durable facts are invalid: {0}")]
    InvalidDurableFacts(String),
    #[error("coordination dispatch is invalid: {0}")]
    InvalidDispatch(String),
    #[error("task handoff is invalid: {0}")]
    InvalidHandoff(String),
    #[error("moderator arbitration is invalid: {0}")]
    InvalidArbitration(String),
    #[error("coordination idempotency key was reused for different intent")]
    IdempotencyConflict,
    #[error("coordination service configuration is invalid")]
    InvalidConfiguration,
    #[error("Agent spawn is invalid: {0}")]
    InvalidAgentSpawn(String),
    #[error(transparent)]
    Storage(#[from] SessionStoreError),
}
