//! Moderator decision application and renewable arbitration recovery.

use sylvander_api::GovernanceCaseId;

use super::{
    ArbitrationCase, ArbitrationState, CoordinationService, CoordinationServiceError,
    MAX_ARBITRATION_RENEWALS, ModeratorDecision, SessionTaskGraph, governance_case_id,
};
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;

impl<S> CoordinationService<S>
where
    S: AgentInstanceStore + CoordinationStore,
{
    /// Validate a fenced moderator decision and atomically apply all of its
    /// durable effects. Exact retries return the already-applied case.
    pub async fn decide_arbitration(
        &self,
        decision: &ModeratorDecision,
        now: i64,
    ) -> Result<ArbitrationCase, CoordinationServiceError> {
        let case = self
            .store
            .arbitration_case(&decision.case_id)
            .await?
            .ok_or(CoordinationServiceError::UnknownArbitration)?;
        if case.state == ArbitrationState::Applied {
            return match self.store.arbitration_decision(&decision.case_id).await? {
                Some(existing) if existing == *decision => Ok(case),
                _ => Err(CoordinationServiceError::IdempotencyConflict),
            };
        }
        if case.state != ArbitrationState::Open {
            return Err(CoordinationServiceError::InvalidArbitration(
                "arbitration case is not open".into(),
            ));
        }
        let membership = self
            .store
            .session_membership(&case.session_id)
            .await?
            .ok_or_else(|| CoordinationServiceError::MissingMembership(case.session_id.clone()))?;
        let topology = self
            .store
            .topology(&case.session_id)
            .await?
            .ok_or_else(|| CoordinationServiceError::MissingTopology(case.session_id.clone()))?;
        let tasks = self
            .store
            .task_graph(&case.session_id)
            .await?
            .unwrap_or_else(|| SessionTaskGraph {
                session_id: case.session_id.clone(),
                membership_revision: membership.governance.membership_revision,
                tasks: Vec::new(),
                dependencies: Vec::new(),
            });
        self.store
            .decide_arbitration(
                decision,
                &membership,
                &tasks,
                topology.topology_revision,
                now,
            )
            .await
            .map_err(Into::into)
    }

    pub async fn arbitration_case(
        &self,
        case_id: &GovernanceCaseId,
    ) -> Result<Option<ArbitrationCase>, CoordinationServiceError> {
        self.store
            .arbitration_case(case_id)
            .await
            .map_err(Into::into)
    }

    pub(super) async fn applied_decision(
        &self,
        case: &ArbitrationCase,
    ) -> Result<ModeratorDecision, CoordinationServiceError> {
        if case.state != ArbitrationState::Applied {
            return Err(CoordinationServiceError::InvalidArbitration(
                "arbitration case is not applied".into(),
            ));
        }
        self.store
            .arbitration_decision(&case.case_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::InvalidDurableFacts(
                    "applied arbitration case has no moderator decision".into(),
                )
            })
    }

    pub(super) async fn current_arbitration(
        &self,
        initial_case_id: GovernanceCaseId,
        now: i64,
    ) -> Result<(GovernanceCaseId, Option<ArbitrationCase>), CoordinationServiceError> {
        let mut case_id = initial_case_id;
        for _ in 0..MAX_ARBITRATION_RENEWALS {
            let Some(case) = self.store.arbitration_case(&case_id).await? else {
                return Ok((case_id, None));
            };
            let expired = match case.state {
                ArbitrationState::Open if case.expires_at <= now => {
                    self.store
                        .expire_arbitration(&case.case_id, case.revision, now)
                        .await?
                }
                ArbitrationState::Expired => case,
                _ => return Ok((case_id, Some(case))),
            };
            case_id = governance_case_id(
                "renewal",
                &(expired.case_id, expired.revision, expired.expires_at),
                expired.membership_revision,
                expired.topology_revision,
            )?;
        }
        Err(CoordinationServiceError::InvalidDurableFacts(
            "arbitration renewal chain exceeds the bounded recovery limit".into(),
        ))
    }
}
