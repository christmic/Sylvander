//! Governed application service for durable inter-Agent coordination.

use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};
use sylvander_api::HandoffId;
use sylvander_api::{AgentInstanceId, CoordinationMessageId, GovernanceCaseId, SessionId, TaskId};

use crate::agent::instance::{
    AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute, HistoryView,
    SessionAgentRole,
};
use crate::coordination::arbitration::{ArbitrationCase, ArbitrationState};
use crate::coordination::governance::{
    GovernanceAssessment, GovernancePolicy, GovernanceSnapshot, ProgressObservation,
    WaitDependency, assess,
};
use crate::coordination::handoff::{HandoffState, TaskHandoff};
use crate::coordination::mailbox::{
    CoordinationMessage, CoordinationMessageKind, MessageClaim, MessageDeliveryState,
};
use crate::coordination::task::SessionTaskGraph;
use crate::coordination::topology::{AgentRelation, AgentRelationKind, SessionTopology};
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::SessionStoreError;

pub const DEFAULT_ARBITRATION_TTL_SECONDS: u64 = 300;

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
    RequiresArbitration {
        case: ArbitrationCase,
        assessment: GovernanceAssessment,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForkAgentRequest {
    pub instance_id: AgentInstanceId,
    pub session_id: SessionId,
    pub parent_instance_id: AgentInstanceId,
    pub base_sequence: u64,
    pub branch_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkAgentOutcome {
    Created(AgentInstance),
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

        let observation_window = self
            .policy
            .stagnation_window
            .max(self.policy.handoff_ping_pong_window)
            .max(1);
        let observations = self
            .store
            .governance_observations(&request.session_id, observation_window)
            .await?;

        let assessment = assess(
            &self.policy,
            &GovernanceSnapshot {
                membership: &membership,
                topology: &topology,
                tasks: &tasks,
                waits: &observations.waits,
                progress: &observations.progress,
                handoffs: &observations.handoffs,
            },
        );
        if !assessment.permits_automatic_progress() {
            let case_id = governance_case_id(
                "message",
                &request,
                membership.governance.membership_revision,
                topology.topology_revision,
            )?;
            if let Some(case) = self.store.arbitration_case(&case_id).await? {
                self.ensure_arbitration_notification(
                    &request.sender_instance_id,
                    request.task_id.as_ref(),
                    &case,
                    &membership,
                    &topology,
                )
                .await?;
                return Ok(DispatchMessageOutcome::RequiresArbitration { case, assessment });
            }
            let ttl = i64::try_from(self.arbitration_ttl_seconds)
                .map_err(|_| CoordinationServiceError::InvalidConfiguration)?;
            let case = ArbitrationCase {
                case_id,
                session_id: request.session_id.clone(),
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
            self.ensure_arbitration_notification(
                &request.sender_instance_id,
                request.task_id.as_ref(),
                &case,
                &membership,
                &topology,
            )
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

    /// Durably fork one child participant without rewriting existing members.
    pub async fn fork_agent(
        &self,
        request: ForkAgentRequest,
        now: i64,
    ) -> Result<ForkAgentOutcome, CoordinationServiceError> {
        if request.branch_id.trim().is_empty() || request.branch_id.len() > 256 {
            return Err(CoordinationServiceError::InvalidAgentSpawn(
                "fork branch identity is invalid".into(),
            ));
        }
        let membership = self
            .store
            .session_membership(&request.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.session_id.clone())
            })?;
        let parent = membership
            .participants
            .iter()
            .find(|participant| participant.instance_id == request.parent_instance_id)
            .ok_or(CoordinationServiceError::UnknownAgent)?;
        ensure_available(&membership, &request.parent_instance_id)?;
        if let Some(existing) = membership
            .participants
            .iter()
            .find(|participant| participant.instance_id == request.instance_id)
        {
            ensure_available(&membership, &existing.instance_id)?;
            return if same_fork_intent(existing, parent, &request) {
                Ok(ForkAgentOutcome::Created(existing.clone()))
            } else {
                Err(CoordinationServiceError::IdempotencyConflict)
            };
        }
        let topology = self
            .store
            .topology(&request.session_id)
            .await?
            .ok_or_else(|| CoordinationServiceError::MissingTopology(request.session_id.clone()))?;
        let fork_sequence = membership
            .participants
            .iter()
            .filter_map(|participant| match &participant.origin {
                AgentInstanceOrigin::Forked {
                    parent_instance_id,
                    fork_sequence,
                } if parent_instance_id == &request.parent_instance_id => Some(*fork_sequence),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(CoordinationServiceError::InvalidAgentSpawn(
                "fork sequence overflow".into(),
            ))?;
        let participant = AgentInstance {
            instance_id: request.instance_id.clone(),
            session_id: request.session_id.clone(),
            definition: parent.definition.clone(),
            origin: AgentInstanceOrigin::Forked {
                parent_instance_id: request.parent_instance_id.clone(),
                fork_sequence,
            },
            role: SessionAgentRole::Worker,
            history_view: HistoryView::ForkSnapshot {
                base_sequence: request.base_sequence,
                branch_id: request.branch_id.clone(),
            },
            approval_route: ApprovalRoute::Parent {
                instance_id: request.parent_instance_id.clone(),
            },
            state: AgentInstanceState::Created,
            lifecycle_revision: 0,
            capability_revision: parent.capability_revision.clone(),
            created_at: now,
            updated_at: now,
        };
        let next_membership_revision = membership
            .governance
            .membership_revision
            .checked_add(1)
            .ok_or(CoordinationServiceError::InvalidAgentSpawn(
                "membership revision overflow".into(),
            ))?;
        let mut participants = membership.participants.clone();
        participants.push(participant.clone());
        let next_membership = crate::session::membership::SessionMembership::new(
            request.session_id.clone(),
            participants,
            crate::session::membership::SessionGovernance {
                membership_revision: next_membership_revision,
                updated_at: now,
                ..membership.governance.clone()
            },
        )
        .map_err(|error| CoordinationServiceError::InvalidAgentSpawn(error.to_string()))?;
        let next_topology_revision = topology.topology_revision.checked_add(1).ok_or(
            CoordinationServiceError::InvalidAgentSpawn("topology revision overflow".into()),
        )?;
        let mut relations = topology.relations.clone();
        relations.push(AgentRelation {
            source: request.parent_instance_id.clone(),
            target: request.instance_id.clone(),
            kind: AgentRelationKind::ParentOf,
            created_at: now,
        });
        let next_topology = SessionTopology::new(
            request.session_id.clone(),
            next_membership_revision,
            next_topology_revision,
            relations,
            now,
            &next_membership,
        )
        .map_err(|error| CoordinationServiceError::InvalidAgentSpawn(error.to_string()))?;
        let mut tasks = self
            .store
            .task_graph(&request.session_id)
            .await?
            .unwrap_or_else(|| SessionTaskGraph {
                session_id: request.session_id.clone(),
                membership_revision: next_membership_revision,
                tasks: Vec::new(),
                dependencies: Vec::new(),
            });
        tasks.membership_revision = next_membership_revision;
        for task in &mut tasks.tasks {
            task.membership_revision = next_membership_revision;
        }
        tasks
            .validate(&next_membership)
            .map_err(|error| CoordinationServiceError::InvalidDurableFacts(error.to_string()))?;
        let observations = self
            .store
            .governance_observations(
                &request.session_id,
                self.policy
                    .stagnation_window
                    .max(self.policy.handoff_ping_pong_window)
                    .max(1),
            )
            .await?;
        let assessment = assess(
            &self.policy,
            &GovernanceSnapshot {
                membership: &next_membership,
                topology: &next_topology,
                tasks: &tasks,
                waits: &observations.waits,
                progress: &observations.progress,
                handoffs: &observations.handoffs,
            },
        );
        if !assessment.permits_automatic_progress() {
            let case_id = governance_case_id(
                "fork",
                &request,
                membership.governance.membership_revision,
                topology.topology_revision,
            )?;
            let case = if let Some(case) = self.store.arbitration_case(&case_id).await? {
                case
            } else {
                let ttl = i64::try_from(self.arbitration_ttl_seconds)
                    .map_err(|_| CoordinationServiceError::InvalidConfiguration)?;
                let case = ArbitrationCase {
                    case_id,
                    session_id: request.session_id.clone(),
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
                case
            };
            self.ensure_arbitration_notification(
                &request.parent_instance_id,
                None,
                &case,
                &membership,
                &topology,
            )
            .await?;
            return Ok(ForkAgentOutcome::RequiresArbitration { case, assessment });
        }
        self.store
            .add_session_participant(
                &participant,
                &next_membership,
                &next_topology,
                membership.governance.membership_revision,
                topology.topology_revision,
            )
            .await?;
        Ok(ForkAgentOutcome::Created(participant))
    }

    /// Commit completion of external attach/provision effects for one spawn intent.
    pub async fn mark_agent_ready(
        &self,
        participant: &AgentInstance,
        now: i64,
    ) -> Result<AgentInstance, CoordinationServiceError> {
        if participant.state != AgentInstanceState::Created {
            return Ok(participant.clone());
        }
        self.store
            .transition_agent_instance(
                &participant.session_id,
                &participant.instance_id,
                participant.lifecycle_revision,
                AgentInstanceState::Ready,
                now,
            )
            .await
            .map_err(Into::into)
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

    /// Apply an idempotent handoff verdict under the selected arbitrator.
    pub async fn decide_handoff(
        &self,
        session_id: &SessionId,
        handoff_id: &HandoffId,
        arbitrator: &AgentInstanceId,
        accept: bool,
        now: i64,
    ) -> Result<TaskHandoff, CoordinationServiceError> {
        let handoff = self
            .store
            .handoff(handoff_id)
            .await?
            .ok_or(CoordinationServiceError::UnknownHandoff)?;
        if &handoff.session_id != session_id {
            return Err(CoordinationServiceError::UnknownHandoff);
        }
        if &handoff.arbitrator_instance_id != arbitrator {
            return Err(CoordinationServiceError::UnauthorizedArbitrator);
        }
        let intended = if accept {
            HandoffState::Accepted
        } else {
            HandoffState::Rejected
        };
        if handoff.state == intended {
            return Ok(handoff);
        }
        if handoff.state != HandoffState::AwaitingArbitration {
            return Err(CoordinationServiceError::InvalidHandoff(
                "handoff is not awaiting arbitration".into(),
            ));
        }
        self.store
            .transition_handoff(handoff_id, arbitrator, intended, handoff.revision, now)
            .await
            .map_err(Into::into)
    }

    /// Lease one durable envelope. Expired claims are recoverable by a later worker.
    pub async fn claim_next_message(
        &self,
        recipient: &AgentInstanceId,
        now: i64,
        lease_seconds: u64,
    ) -> Result<Option<MessageClaim>, CoordinationServiceError> {
        if self.policy.max_message_delivery_attempts == 0 {
            return Err(CoordinationServiceError::InvalidConfiguration);
        }
        let claim = self
            .store
            .claim_message(recipient, now, lease_seconds)
            .await?;
        let Some(claim) = claim else {
            return Ok(None);
        };
        if claim.message.delivery_attempts > self.policy.max_message_delivery_attempts {
            self.store
                .finish_message_claim(
                    &claim.message.message_id,
                    recipient,
                    claim.lease_epoch,
                    MessageDeliveryState::DeadLetter,
                    now,
                )
                .await?;
            return Ok(None);
        }
        Ok(Some(claim))
    }

    /// Commit delivery under the exact claim epoch before exposing its payload.
    pub async fn mark_message_delivered(
        &self,
        claim: &MessageClaim,
        now: i64,
    ) -> Result<CoordinationMessage, CoordinationServiceError> {
        self.store
            .finish_message_claim(
                &claim.message.message_id,
                &claim.message.recipient_instance_id,
                claim.lease_epoch,
                MessageDeliveryState::Delivered,
                now,
            )
            .await
            .map_err(Into::into)
    }

    /// Persist recipient acknowledgement using the delivered revision as a fence.
    pub async fn acknowledge_message(
        &self,
        message: &CoordinationMessage,
        recipient: &AgentInstanceId,
        now: i64,
    ) -> Result<CoordinationMessage, CoordinationServiceError> {
        self.store
            .acknowledge_message(&message.message_id, recipient, message.revision, now)
            .await
            .map_err(Into::into)
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

fn same_fork_intent(
    existing: &AgentInstance,
    parent: &AgentInstance,
    request: &ForkAgentRequest,
) -> bool {
    existing.session_id == request.session_id
        && existing.definition == parent.definition
        && existing.role == SessionAgentRole::Worker
        && existing.capability_revision == parent.capability_revision
        && matches!(
            &existing.origin,
            AgentInstanceOrigin::Forked {
                parent_instance_id,
                ..
            } if parent_instance_id == &request.parent_instance_id
        )
        && existing.history_view
            == (HistoryView::ForkSnapshot {
                base_sequence: request.base_sequence,
                branch_id: request.branch_id.clone(),
            })
        && existing.approval_route
            == (ApprovalRoute::Parent {
                instance_id: request.parent_instance_id.clone(),
            })
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
    #[error("coordination references an unknown handoff")]
    UnknownHandoff,
    #[error("handoff decision was not issued by its governed arbitrator")]
    UnauthorizedArbitrator,
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
    #[error("Agent spawn is invalid: {0}")]
    InvalidAgentSpawn(String),
    #[error(transparent)]
    Storage(#[from] SessionStoreError),
}
