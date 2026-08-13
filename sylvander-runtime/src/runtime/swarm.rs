use std::collections::HashSet;

use sylvander_api::{AgentInstanceId, SessionId};

use super::{DefinedAgentJoinRequest, Runtime, RuntimeError};
use crate::agent::run::AuthenticatedSession;
use crate::coordination::arbitration::ArbitrationCase;
use crate::coordination::service::{
    DefineAgentOutcome, ForkAgentOutcome, ForkAgentRequest, RelateAgentsOutcome,
    RelateAgentsRequest,
};

/// One member intent in a recoverable Swarm composition saga.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwarmMemberPlan {
    Fork {
        instance_id: AgentInstanceId,
        parent_instance_id: AgentInstanceId,
        branch_id: String,
    },
    Defined(Box<DefinedAgentJoinRequest>),
}

impl SwarmMemberPlan {
    fn instance_id(&self) -> &AgentInstanceId {
        match self {
            Self::Fork { instance_id, .. } => instance_id,
            Self::Defined(request) => &request.instance_id,
        }
    }

    fn sponsor(&self) -> &AgentInstanceId {
        match self {
            Self::Fork {
                parent_instance_id, ..
            } => parent_instance_id,
            Self::Defined(request) => &request.sponsor_instance_id,
        }
    }
}

/// Declarative, replayable composition. Runtime admits direct members in
/// stable identity order before applying peer/reviewer relations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmCompositionPlan {
    pub session_id: SessionId,
    pub requested_by: AgentInstanceId,
    pub members: Vec<SwarmMemberPlan>,
    pub relations: Vec<RelateAgentsRequest>,
}

impl SwarmCompositionPlan {
    fn ordered_members(&self) -> Result<Vec<&SwarmMemberPlan>, RuntimeError> {
        if self.members.is_empty() {
            return Err(invalid_plan("Swarm requires at least one member"));
        }
        let mut identities = HashSet::with_capacity(self.members.len());
        for member in &self.members {
            if member.instance_id() == &self.requested_by
                || !identities.insert(member.instance_id())
            {
                return Err(invalid_plan("Swarm member identities must be unique"));
            }
            if member.sponsor() != &self.requested_by {
                return Err(invalid_plan(
                    "Swarm members require the authenticated actor as direct sponsor",
                ));
            }
            if let SwarmMemberPlan::Defined(request) = member
                && request.session_id != self.session_id
            {
                return Err(invalid_plan("defined member belongs to another Session"));
            }
        }
        for relation in &self.relations {
            if relation.session_id != self.session_id
                || relation.requested_by != self.requested_by
                || relation.source == relation.target
            {
                return Err(invalid_plan("Swarm relation identity is invalid"));
            }
        }

        let mut ordered = self.members.iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| left.instance_id().0.cmp(&right.instance_id().0));
        Ok(ordered)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SwarmCompositionReceipt {
    pub admitted_members: Vec<AgentInstanceId>,
    pub applied_relations: u32,
    pub moderator_authorizations: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwarmCompositionOutcome {
    Applied(SwarmCompositionReceipt),
    RequiresArbitration {
        receipt: SwarmCompositionReceipt,
        case: ArbitrationCase,
    },
    RejectedByModerator {
        receipt: SwarmCompositionReceipt,
        case: ArbitrationCase,
    },
}

impl Runtime {
    /// Compose a Swarm as an idempotent saga. Every member is directly
    /// sponsored by `actor`; descendants must be composed by that child under
    /// its own unforgeable run authority. If governance pauses the plan, the
    /// receipt states the durable prefix and a retry resumes from it.
    pub async fn compose_agent_swarm(
        &self,
        actor: &AuthenticatedSession,
        plan: SwarmCompositionPlan,
    ) -> Result<SwarmCompositionOutcome, RuntimeError> {
        super::validate_coordination_actor(actor, &plan.session_id, &plan.requested_by)?;
        let ordered = plan.ordered_members()?;
        let mut receipt = SwarmCompositionReceipt::default();
        for member in ordered {
            let outcome = match member {
                SwarmMemberPlan::Fork {
                    instance_id,
                    parent_instance_id,
                    branch_id,
                } => self
                    .fork_agent_instance(
                        actor,
                        ForkAgentRequest {
                            instance_id: instance_id.clone(),
                            session_id: plan.session_id.clone(),
                            parent_instance_id: parent_instance_id.clone(),
                            branch_id: branch_id.clone(),
                        },
                    )
                    .await?
                    .into_swarm(),
                SwarmMemberPlan::Defined(request) => {
                    Box::pin(self.define_agent_instance(actor, request.as_ref().clone()))
                        .await?
                        .into_swarm()
                }
            };
            match outcome {
                MemberOutcome::Admitted { id, moderated } => {
                    receipt.admitted_members.push(id);
                    receipt.moderator_authorizations = receipt
                        .moderator_authorizations
                        .saturating_add(u32::from(moderated));
                }
                MemberOutcome::RequiresArbitration(case) => {
                    return Ok(SwarmCompositionOutcome::RequiresArbitration { receipt, case });
                }
                MemberOutcome::Rejected(case) => {
                    return Ok(SwarmCompositionOutcome::RejectedByModerator { receipt, case });
                }
            }
        }
        for relation in plan.relations {
            match self.relate_agent_instances(actor, relation).await? {
                RelateAgentsOutcome::Applied(_) => {}
                RelateAgentsOutcome::AppliedByModerator { .. } => {
                    receipt.moderator_authorizations =
                        receipt.moderator_authorizations.saturating_add(1);
                }
                RelateAgentsOutcome::RequiresArbitration { case, .. } => {
                    return Ok(SwarmCompositionOutcome::RequiresArbitration { receipt, case });
                }
                RelateAgentsOutcome::RejectedByModerator { case, .. } => {
                    return Ok(SwarmCompositionOutcome::RejectedByModerator { receipt, case });
                }
            }
            receipt.applied_relations = receipt.applied_relations.saturating_add(1);
        }
        Ok(SwarmCompositionOutcome::Applied(receipt))
    }
}

enum MemberOutcome {
    Admitted {
        id: AgentInstanceId,
        moderated: bool,
    },
    RequiresArbitration(ArbitrationCase),
    Rejected(ArbitrationCase),
}

trait IntoSwarmMemberOutcome {
    fn into_swarm(self) -> MemberOutcome;
}

impl IntoSwarmMemberOutcome for ForkAgentOutcome {
    fn into_swarm(self) -> MemberOutcome {
        match self {
            Self::Created(agent) => MemberOutcome::Admitted {
                id: agent.instance_id,
                moderated: false,
            },
            Self::CreatedByModerator { participant, .. } => MemberOutcome::Admitted {
                id: participant.instance_id,
                moderated: true,
            },
            Self::RequiresArbitration { case, .. } => MemberOutcome::RequiresArbitration(case),
            Self::RejectedByModerator { case, .. } => MemberOutcome::Rejected(case),
        }
    }
}

impl IntoSwarmMemberOutcome for DefineAgentOutcome {
    fn into_swarm(self) -> MemberOutcome {
        match self {
            Self::Created(agent) => MemberOutcome::Admitted {
                id: agent.instance_id,
                moderated: false,
            },
            Self::CreatedByModerator { participant, .. } => MemberOutcome::Admitted {
                id: participant.instance_id,
                moderated: true,
            },
            Self::RequiresArbitration { case, .. } => MemberOutcome::RequiresArbitration(case),
            Self::RejectedByModerator { case, .. } => MemberOutcome::Rejected(case),
        }
    }
}

fn invalid_plan(message: &str) -> RuntimeError {
    RuntimeError::Coordination(message.to_owned())
}

#[cfg(test)]
#[path = "../../tests/unit/runtime_swarm.rs"]
mod tests;
