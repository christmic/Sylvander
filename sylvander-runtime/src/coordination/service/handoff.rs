//! Governed task ownership transfer and arbitrator decisions.

use sylvander_api::{AgentInstanceId, HandoffId, SessionId};

use super::{
    CoordinationService, CoordinationServiceError, HandoffState, ProposeHandoffRequest,
    TaskHandoff, ensure_available,
};
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;

impl<S> CoordinationService<S>
where
    S: AgentInstanceStore + CoordinationStore,
{
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
