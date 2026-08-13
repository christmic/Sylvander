//! Governed admission of independently defined Agent participants.

use super::{
    AgentInstance, AgentInstanceOrigin, AgentInstanceState, AgentRelation, AgentRelationKind,
    ApprovalRoute, ArbitrationCase, ArbitrationState, CoordinationService,
    CoordinationServiceError, DefineAgentOutcome, DefineAgentRequest, GovernanceSnapshot,
    HistoryView, ModeratorVerdict, SessionTaskGraph, SessionTopology, assess, ensure_available,
    governance_case_id,
};
use crate::storage::agent_instance::{
    AgentInstanceConfig, AgentInstanceConfigSeed, AgentInstanceStore,
};
use crate::storage::coordination::CoordinationStore;

impl<S> CoordinationService<S>
where
    S: AgentInstanceStore + CoordinationStore,
{
    /// Admit one separately defined Agent under the current governance graph.
    pub async fn define_agent(
        &self,
        request: DefineAgentRequest,
        now: i64,
    ) -> Result<DefineAgentOutcome, CoordinationServiceError> {
        if request.role.is_root_moderator()
            || request.capability_revision.trim().is_empty()
            || request.effective_config.agent_id != request.definition.agent_id
            || request.effective_config.agent_revision != request.definition.revision
        {
            return Err(CoordinationServiceError::InvalidAgentSpawn(
                "defined Agent identity, role, or configuration is invalid".into(),
            ));
        }
        let membership = self
            .store
            .session_membership(&request.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.session_id.clone())
            })?;
        ensure_available(&membership, &request.sponsor_instance_id)?;
        if let Some(existing) = membership
            .participants
            .iter()
            .find(|participant| participant.instance_id == request.instance_id)
        {
            ensure_available(&membership, &existing.instance_id)?;
            let config = self
                .store
                .agent_instance_config(&request.session_id, &request.instance_id)
                .await?;
            return if same_defined_intent(existing, config.as_ref(), &request, &membership) {
                Ok(DefineAgentOutcome::Created(existing.clone()))
            } else {
                Err(CoordinationServiceError::IdempotencyConflict)
            };
        }
        let topology = self
            .store
            .topology(&request.session_id)
            .await?
            .ok_or_else(|| CoordinationServiceError::MissingTopology(request.session_id.clone()))?;
        let participant = AgentInstance {
            instance_id: request.instance_id.clone(),
            session_id: request.session_id.clone(),
            definition: request.definition.clone(),
            origin: AgentInstanceOrigin::Defined,
            role: request.role.clone(),
            history_view: HistoryView::SharedLane { cursor: 0 },
            approval_route: ApprovalRoute::Moderator {
                instance_id: membership.governance.moderator_instance_id.clone(),
            },
            state: AgentInstanceState::Created,
            lifecycle_revision: 0,
            capability_revision: request.capability_revision.clone(),
            created_at: now,
            updated_at: now,
        };
        let membership_revision = membership
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
                membership_revision,
                updated_at: now,
                ..membership.governance.clone()
            },
        )
        .map_err(|error| CoordinationServiceError::InvalidAgentSpawn(error.to_string()))?;
        let topology_revision = topology.topology_revision.checked_add(1).ok_or(
            CoordinationServiceError::InvalidAgentSpawn("topology revision overflow".into()),
        )?;
        let mut relations = topology.relations.clone();
        relations.push(AgentRelation {
            source: request.sponsor_instance_id.clone(),
            target: request.instance_id.clone(),
            kind: AgentRelationKind::ParentOf,
            created_at: now,
        });
        let next_topology = SessionTopology::new(
            request.session_id.clone(),
            membership_revision,
            topology_revision,
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
                membership_revision,
                tasks: Vec::new(),
                dependencies: Vec::new(),
            });
        tasks.membership_revision = membership_revision;
        for task in &mut tasks.tasks {
            task.membership_revision = membership_revision;
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
                        "define",
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
                        return Ok(DefineAgentOutcome::RejectedByModerator { case, decision });
                    }
                } else {
                    self.ensure_arbitration_notification(
                        &request.sponsor_instance_id,
                        None,
                        &case,
                        &membership,
                        &topology,
                    )
                    .await?;
                    return Ok(DefineAgentOutcome::RequiresArbitration { case, assessment });
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
                    &request.sponsor_instance_id,
                    None,
                    &case,
                    &membership,
                    &topology,
                )
                .await?;
                return Ok(DefineAgentOutcome::RequiresArbitration { case, assessment });
            }
        }
        let participant = self
            .store
            .add_session_participant(
                &participant,
                AgentInstanceConfigSeed::Exact(Box::new(AgentInstanceConfig {
                    session_id: request.session_id,
                    instance_id: request.instance_id,
                    config_revision: 0,
                    effective: request.effective_config,
                    updated_at: now,
                })),
                &next_membership,
                &next_topology,
                membership.governance.membership_revision,
                topology.topology_revision,
            )
            .await?;
        Ok(match moderator_authorization {
            Some(decision) => DefineAgentOutcome::CreatedByModerator {
                participant,
                decision,
            },
            None => DefineAgentOutcome::Created(participant),
        })
    }
}

fn same_defined_intent(
    existing: &AgentInstance,
    config: Option<&AgentInstanceConfig>,
    request: &DefineAgentRequest,
    membership: &crate::session::membership::SessionMembership,
) -> bool {
    existing.session_id == request.session_id
        && existing.definition == request.definition
        && existing.origin == AgentInstanceOrigin::Defined
        && existing.role == request.role
        && existing.capability_revision == request.capability_revision
        && existing.history_view == (HistoryView::SharedLane { cursor: 0 })
        && existing.approval_route
            == (ApprovalRoute::Moderator {
                instance_id: membership.governance.moderator_instance_id.clone(),
            })
        && config.is_some_and(|config| {
            config.config_revision == 0 && config.effective == request.effective_config
        })
}
