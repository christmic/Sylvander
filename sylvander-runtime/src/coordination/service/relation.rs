//! Governed evolution of non-ownership Agent relationships.

use super::{
    AgentRelation, AgentRelationKind, ArbitrationCase, ArbitrationState, CoordinationService,
    CoordinationServiceError, GovernanceSnapshot, ModeratorVerdict, RelateAgentsOutcome,
    RelateAgentsRequest, SessionTaskGraph, SessionTopology, assess, ensure_available,
    governance_case_id,
};
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;

impl<S> CoordinationService<S>
where
    S: AgentInstanceStore + CoordinationStore,
{
    /// Add a Peer or Reviews edge without mutating the ownership tree.
    pub async fn relate_agents(
        &self,
        request: RelateAgentsRequest,
        now: i64,
    ) -> Result<RelateAgentsOutcome, CoordinationServiceError> {
        if request.kind == AgentRelationKind::ParentOf || request.source == request.target {
            return Err(CoordinationServiceError::InvalidAgentSpawn(
                "dynamic relationships cannot rewrite Agent ownership".into(),
            ));
        }
        let membership = self
            .store
            .session_membership(&request.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.session_id.clone())
            })?;
        ensure_available(&membership, &request.requested_by)?;
        ensure_available(&membership, &request.source)?;
        ensure_available(&membership, &request.target)?;
        if request.requested_by != request.source
            && request.requested_by != membership.governance.moderator_instance_id
        {
            return Err(CoordinationServiceError::UnauthorizedActor);
        }
        let topology = self
            .store
            .topology(&request.session_id)
            .await?
            .ok_or_else(|| CoordinationServiceError::MissingTopology(request.session_id.clone()))?;
        let proposed = AgentRelation {
            source: request.source.clone(),
            target: request.target.clone(),
            kind: request.kind,
            created_at: now,
        };
        if topology.relations.iter().any(|relation| {
            relation.kind == proposed.kind
                && ((relation.source == proposed.source && relation.target == proposed.target)
                    || (proposed.kind == AgentRelationKind::Peer
                        && relation.source == proposed.target
                        && relation.target == proposed.source))
        }) {
            return Ok(RelateAgentsOutcome::Applied(topology));
        }
        let next_revision = topology.topology_revision.checked_add(1).ok_or(
            CoordinationServiceError::InvalidAgentSpawn("topology revision overflow".into()),
        )?;
        let mut relations = topology.relations.clone();
        relations.push(proposed);
        let next_topology = SessionTopology::new(
            request.session_id.clone(),
            membership.governance.membership_revision,
            next_revision,
            relations,
            now,
            &membership,
        )
        .map_err(|error| CoordinationServiceError::InvalidAgentSpawn(error.to_string()))?;
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
                membership: &membership,
                topology: &next_topology,
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
                        "relate",
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
                        return Ok(RelateAgentsOutcome::RejectedByModerator { case, decision });
                    }
                } else {
                    self.ensure_arbitration_notification(
                        &request.requested_by,
                        None,
                        &case,
                        &membership,
                        &topology,
                    )
                    .await?;
                    return Ok(RelateAgentsOutcome::RequiresArbitration { case, assessment });
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
                    &request.requested_by,
                    None,
                    &case,
                    &membership,
                    &topology,
                )
                .await?;
                return Ok(RelateAgentsOutcome::RequiresArbitration { case, assessment });
            }
        }
        self.store
            .save_topology(
                &next_topology,
                &membership,
                Some(topology.topology_revision),
            )
            .await?;
        Ok(match moderator_authorization {
            Some(decision) => RelateAgentsOutcome::AppliedByModerator {
                topology: next_topology,
                decision,
            },
            None => RelateAgentsOutcome::Applied(next_topology),
        })
    }
}
