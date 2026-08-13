//! Governed message dispatch, mailbox delivery, and interrupted-turn escalation.

use sylvander_api::AgentInstanceId;

use super::{
    AgentMessageTurn, ArbitrationCase, ArbitrationState, CoordinationMessage, CoordinationService,
    CoordinationServiceError, DispatchMessageOutcome, DispatchMessageRequest, GovernanceFinding,
    GovernanceSnapshot, MessageClaim, MessageDeliveryState, ModeratorVerdict, SessionTaskGraph,
    assess, ensure_available, governance_case_id, same_dispatch_intent,
};
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;

impl<S> CoordinationService<S>
where
    S: AgentInstanceStore + CoordinationStore,
{
    pub async fn coordination_message(
        &self,
        message_id: &sylvander_api::CoordinationMessageId,
    ) -> Result<Option<CoordinationMessage>, CoordinationServiceError> {
        self.store.message(message_id).await.map_err(Into::into)
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
        let mut moderator_authorization = None;
        if !assessment.permits_automatic_progress() {
            let (case_id, existing_case) = self
                .current_arbitration(
                    governance_case_id(
                        "message",
                        &request,
                        membership.governance.membership_revision,
                        topology.topology_revision,
                    )?,
                    now,
                )
                .await?;
            if let Some(case) = existing_case {
                if case.state == ArbitrationState::Applied {
                    let decision = self.applied_decision(&case).await?;
                    if matches!(
                        decision.verdict,
                        ModeratorVerdict::ContinueWithConditions { .. }
                    ) {
                        moderator_authorization = Some(decision);
                    } else {
                        return Ok(DispatchMessageOutcome::RejectedByModerator { case, decision });
                    }
                } else {
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
                Ok(match moderator_authorization {
                    Some(decision) => DispatchMessageOutcome::EnqueuedByModerator {
                        message: existing,
                        decision,
                    },
                    None => DispatchMessageOutcome::Enqueued(existing),
                })
            } else {
                Err(CoordinationServiceError::IdempotencyConflict)
            };
        }
        self.store
            .enqueue_message(&message, &membership, &topology, now)
            .await?;
        Ok(match moderator_authorization {
            Some(decision) => DispatchMessageOutcome::EnqueuedByModerator { message, decision },
            None => DispatchMessageOutcome::Enqueued(message),
        })
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

    /// Persist the exact future turn before an automatic delivery is exposed.
    pub async fn prepare_message_turn(
        &self,
        claim: &MessageClaim,
        turn_id: &str,
        now: i64,
    ) -> Result<(CoordinationMessage, AgentMessageTurn), CoordinationServiceError> {
        self.store
            .prepare_message_turn(claim, turn_id, now)
            .await
            .map_err(Into::into)
    }

    /// Persist a hard-stop case for a mailbox turn whose durable state cannot
    /// be resumed automatically, then wake the fenced moderator mailbox.
    pub async fn escalate_mailbox_turn(
        &self,
        message: &CoordinationMessage,
        receipt: &AgentMessageTurn,
        now: i64,
    ) -> Result<ArbitrationCase, CoordinationServiceError> {
        if message.message_id != receipt.message_id
            || message.session_id != receipt.session_id
            || message.recipient_instance_id != receipt.recipient_instance_id
        {
            return Err(CoordinationServiceError::InvalidDispatch(
                "mailbox turn receipt mismatch".into(),
            ));
        }
        let membership = self
            .store
            .session_membership(&message.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(message.session_id.clone())
            })?;
        let topology = self
            .store
            .topology(&message.session_id)
            .await?
            .ok_or_else(|| CoordinationServiceError::MissingTopology(message.session_id.clone()))?;
        let (case_id, existing_case) = self
            .current_arbitration(
                governance_case_id(
                    "mailbox-turn",
                    &(message.message_id.clone(), receipt.turn_id.clone()),
                    membership.governance.membership_revision,
                    topology.topology_revision,
                )?,
                now,
            )
            .await?;
        let case = if let Some(existing) = existing_case {
            existing
        } else {
            let ttl = i64::try_from(self.arbitration_ttl_seconds)
                .map_err(|_| CoordinationServiceError::InvalidConfiguration)?;
            let case = ArbitrationCase {
                case_id,
                session_id: message.session_id.clone(),
                moderator_instance_id: membership.governance.moderator_instance_id.clone(),
                membership_revision: membership.governance.membership_revision,
                topology_revision: topology.topology_revision,
                moderator_lease_epoch: membership.governance.lease_epoch,
                moderator_fencing_token: membership.governance.fencing_token,
                findings: vec![GovernanceFinding::MailboxTurnUnresolved {
                    agent_instance_id: message.recipient_instance_id.clone(),
                    message_id: message.message_id.clone(),
                }],
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
            &message.recipient_instance_id,
            message.task_id.as_ref(),
            &case,
            &membership,
            &topology,
        )
        .await?;
        Ok(case)
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
}
