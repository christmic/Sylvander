//! Governed fork construction and durable Agent activation boundaries.

use super::{
    AgentInstance, AgentInstanceOrigin, AgentInstanceState, AgentRelation, AgentRelationKind,
    ApprovalRoute, ArbitrationCase, ArbitrationState, CoordinationService,
    CoordinationServiceError, ForkAgentOutcome, ForkAgentRequest, GovernanceSnapshot, HistoryView,
    ModeratorVerdict, SessionAgentRole, SessionTaskGraph, SessionTopology, assess,
    ensure_available, governance_case_id,
};
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;

impl<S> CoordinationService<S>
where
    S: AgentInstanceStore + CoordinationStore,
{
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
                base_sequence: 0,
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
        let mut moderator_authorization = None;
        if !assessment.permits_automatic_progress() {
            let (case_id, existing_case) = self
                .current_arbitration(
                    governance_case_id(
                        "fork",
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
                        return Ok(ForkAgentOutcome::RejectedByModerator { case, decision });
                    }
                } else {
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
                    &request.parent_instance_id,
                    None,
                    &case,
                    &membership,
                    &topology,
                )
                .await?;
                return Ok(ForkAgentOutcome::RequiresArbitration { case, assessment });
            }
        }
        let participant = self
            .store
            .add_session_participant(
                &participant,
                &next_membership,
                &next_topology,
                membership.governance.membership_revision,
                topology.topology_revision,
            )
            .await?;
        Ok(match moderator_authorization {
            Some(decision) => ForkAgentOutcome::CreatedByModerator {
                participant,
                decision,
            },
            None => ForkAgentOutcome::Created(participant),
        })
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
        && matches!(
            &existing.history_view,
            HistoryView::ForkSnapshot { branch_id, .. } if branch_id == &request.branch_id
        )
        && existing.approval_route
            == (ApprovalRoute::Parent {
                instance_id: request.parent_instance_id.clone(),
            })
}
