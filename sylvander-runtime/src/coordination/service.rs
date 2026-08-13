//! Governed application service for durable inter-Agent coordination.

use std::sync::Arc;

use sylvander_api::HandoffId;
use sylvander_api::{AgentInstanceId, CoordinationMessageId, GovernanceCaseId, SessionId, TaskId};

use crate::agent::instance::AgentInstanceState;
use crate::coordination::arbitration::{ArbitrationCase, ArbitrationState};
use crate::coordination::governance::{
    GovernanceAssessment, GovernancePolicy, GovernanceSnapshot, assess,
};
use crate::coordination::handoff::{HandoffState, TaskHandoff};
use crate::coordination::mailbox::{
    CoordinationMessage, CoordinationMessageKind, MessageDeliveryState,
};
use crate::coordination::task::SessionTaskGraph;
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::SessionStoreError;

/// Stable caller intent. Runtime derives every governance fact and route.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    RequiresArbitration {
        case: ArbitrationCase,
        assessment: GovernanceAssessment,
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

    /// Resolve current durable facts, govern the dispatch, then persist its outcome.
    pub async fn dispatch_message(
        &self,
        request: DispatchMessageRequest,
        now: i64,
    ) -> Result<DispatchMessageOutcome, CoordinationServiceError> {
        if self.arbitration_ttl_seconds == 0 {
            return Err(CoordinationServiceError::InvalidConfiguration);
        }
        let membership = self
            .store
            .session_membership(&request.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.session_id.clone())
            })?;
        membership
            .validate()
            .map_err(|error| CoordinationServiceError::InvalidDurableFacts(error.to_string()))?;
        ensure_available(&membership, &request.sender_instance_id)?;
        ensure_available(&membership, &request.recipient_instance_id)?;

        let topology = self
            .store
            .topology(&request.session_id)
            .await?
            .ok_or_else(|| CoordinationServiceError::MissingTopology(request.session_id.clone()))?;
        topology
            .validate(&membership)
            .map_err(|error| CoordinationServiceError::InvalidDurableFacts(error.to_string()))?;
        let tasks = self
            .store
            .task_graph(&request.session_id)
            .await?
            .unwrap_or_else(|| SessionTaskGraph {
                session_id: request.session_id.clone(),
                membership_revision: membership.governance.membership_revision,
                tasks: Vec::new(),
                dependencies: Vec::new(),
            });
        tasks
            .validate(&membership)
            .map_err(|error| CoordinationServiceError::InvalidDurableFacts(error.to_string()))?;
        if request
            .task_id
            .as_ref()
            .is_some_and(|task_id| !tasks.tasks.iter().any(|task| &task.task_id == task_id))
        {
            return Err(CoordinationServiceError::UnknownTask);
        }

        let assessment = assess(
            &self.policy,
            &GovernanceSnapshot {
                membership: &membership,
                topology: &topology,
                tasks: &tasks,
                waits: &[],
                progress: &[],
                handoffs: &[],
            },
        );
        if !assessment.permits_automatic_progress() {
            let case_id = GovernanceCaseId::new(format!(
                "message:{}:membership:{}:topology:{}",
                request.message_id.0,
                membership.governance.membership_revision,
                topology.topology_revision
            ));
            if let Some(case) = self.store.arbitration_case(&case_id).await? {
                return Ok(DispatchMessageOutcome::RequiresArbitration { case, assessment });
            }
            let ttl = i64::try_from(self.arbitration_ttl_seconds)
                .map_err(|_| CoordinationServiceError::InvalidConfiguration)?;
            let case = ArbitrationCase {
                case_id,
                session_id: request.session_id,
                moderator_instance_id: membership.governance.moderator_instance_id.clone(),
                membership_revision: membership.governance.membership_revision,
                topology_revision: topology.topology_revision,
                moderator_lease_epoch: membership.governance.lease_epoch,
                moderator_fencing_token: membership.governance.fencing_token,
                findings: assessment.findings.clone(),
                state: ArbitrationState::Open,
                revision: 0,
                expires_at: now
                    .checked_add(ttl)
                    .ok_or(CoordinationServiceError::InvalidConfiguration)?,
                created_at: now,
                updated_at: now,
            };
            self.store
                .create_arbitration_case(&case, &membership, &topology, now)
                .await?;
            return Ok(DispatchMessageOutcome::RequiresArbitration { case, assessment });
        }

        let route = topology
            .route_between(&request.sender_instance_id, &request.recipient_instance_id)
            .ok_or(CoordinationServiceError::Unroutable)?;
        let message = CoordinationMessage {
            message_id: request.message_id,
            session_id: request.session_id,
            sender_instance_id: request.sender_instance_id,
            recipient_instance_id: request.recipient_instance_id,
            task_id: request.task_id,
            kind: request.kind,
            payload: request.payload,
            topology_revision: topology.topology_revision,
            route,
            max_hops: request.max_hops,
            state: MessageDeliveryState::Pending,
            delivery_attempts: 0,
            revision: 0,
            expires_at: request.expires_at,
            created_at: now,
            updated_at: now,
        };
        message
            .validate_new(&topology, &membership, now)
            .map_err(|error| CoordinationServiceError::InvalidDispatch(error.to_string()))?;
        if let Some(existing) = self.store.message(&message.message_id).await? {
            return if same_dispatch_intent(&existing, &message) {
                Ok(DispatchMessageOutcome::Enqueued(existing))
            } else {
                Err(CoordinationServiceError::IdempotencyConflict)
            };
        }
        self.store
            .enqueue_message(&message, &membership, &topology, now)
            .await?;
        Ok(DispatchMessageOutcome::Enqueued(message))
    }

    /// Persist and route a task ownership transfer to its governed arbitrator.
    pub async fn propose_handoff(
        &self,
        request: ProposeHandoffRequest,
        now: i64,
    ) -> Result<TaskHandoff, CoordinationServiceError> {
        let membership = self
            .store
            .session_membership(&request.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.session_id.clone())
            })?;
        membership
            .validate()
            .map_err(|error| CoordinationServiceError::InvalidDurableFacts(error.to_string()))?;
        ensure_available(&membership, &request.from_instance_id)?;
        ensure_available(&membership, &request.to_instance_id)?;
        ensure_available(&membership, &request.requested_by)?;
        let topology = self
            .store
            .topology(&request.session_id)
            .await?
            .ok_or_else(|| CoordinationServiceError::MissingTopology(request.session_id.clone()))?;
        topology
            .validate(&membership)
            .map_err(|error| CoordinationServiceError::InvalidDurableFacts(error.to_string()))?;
        let task = self
            .store
            .task(&request.task_id)
            .await?
            .ok_or(CoordinationServiceError::UnknownTask)?;
        let arbitrator_instance_id = topology.arbitrator_for(
            &request.from_instance_id,
            &request.to_instance_id,
            &membership,
        );
        ensure_available(&membership, &arbitrator_instance_id)?;
        let proposal = TaskHandoff {
            handoff_id: request.handoff_id,
            session_id: request.session_id,
            task_id: request.task_id,
            from_instance_id: request.from_instance_id,
            to_instance_id: request.to_instance_id,
            requested_by: request.requested_by,
            arbitrator_instance_id,
            task_revision: task.revision,
            topology_revision: topology.topology_revision,
            reason: request.reason,
            state: HandoffState::Proposed,
            revision: 0,
            expires_at: request.expires_at,
            created_at: now,
            updated_at: now,
        };
        proposal
            .validate_proposal(&task, &topology, &membership, now)
            .map_err(|error| CoordinationServiceError::InvalidHandoff(error.to_string()))?;

        let durable = if let Some(existing) = self.store.handoff(&proposal.handoff_id).await? {
            if !same_handoff_intent(&existing, &proposal) {
                return Err(CoordinationServiceError::IdempotencyConflict);
            }
            existing
        } else {
            self.store
                .create_handoff(&proposal, &membership, &topology, now)
                .await?;
            proposal
        };
        if durable.state == HandoffState::Proposed {
            return self
                .store
                .transition_handoff(
                    &durable.handoff_id,
                    &durable.requested_by,
                    HandoffState::AwaitingArbitration,
                    durable.revision,
                    now,
                )
                .await
                .map_err(Into::into);
        }
        Ok(durable)
    }
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

fn same_handoff_intent(existing: &TaskHandoff, proposed: &TaskHandoff) -> bool {
    existing.handoff_id == proposed.handoff_id
        && existing.session_id == proposed.session_id
        && existing.task_id == proposed.task_id
        && existing.from_instance_id == proposed.from_instance_id
        && existing.to_instance_id == proposed.to_instance_id
        && existing.requested_by == proposed.requested_by
        && existing.arbitrator_instance_id == proposed.arbitrator_instance_id
        && existing.task_revision == proposed.task_revision
        && existing.topology_revision == proposed.topology_revision
        && existing.reason == proposed.reason
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
    #[error("recipient is not reachable in the governed topology")]
    Unroutable,
    #[error("coordination durable facts are invalid: {0}")]
    InvalidDurableFacts(String),
    #[error("coordination dispatch is invalid: {0}")]
    InvalidDispatch(String),
    #[error("task handoff is invalid: {0}")]
    InvalidHandoff(String),
    #[error("coordination idempotency key was reused for different intent")]
    IdempotencyConflict,
    #[error("coordination service configuration is invalid")]
    InvalidConfiguration,
    #[error(transparent)]
    Storage(#[from] SessionStoreError),
}
