use super::*;
use crate::agent::definition::AgentId;
use crate::agent::instance::SessionAgentRole;
use sylvander_api::SessionConfigOverrides;

fn defined(id: &str, sponsor: &str) -> SwarmMemberPlan {
    SwarmMemberPlan::Defined(Box::new(DefinedAgentJoinRequest {
        instance_id: AgentInstanceId::new(id),
        session_id: SessionId::new("session"),
        sponsor_instance_id: AgentInstanceId::new(sponsor),
        agent_id: AgentId::new("reviewer"),
        agent_revision: 1,
        role: SessionAgentRole::Reviewer,
        config_overrides: SessionConfigOverrides::default(),
    }))
}

#[test]
fn members_are_ordered_deterministically_not_by_input_order() {
    let plan = SwarmCompositionPlan {
        session_id: SessionId::new("session"),
        requested_by: AgentInstanceId::new("moderator"),
        members: vec![
            defined("worker-z", "moderator"),
            defined("worker-a", "moderator"),
        ],
        relations: Vec::new(),
    };
    let ordered = plan.ordered_members().unwrap();
    assert_eq!(ordered[0].instance_id(), &AgentInstanceId::new("worker-a"));
    assert_eq!(ordered[1].instance_id(), &AgentInstanceId::new("worker-z"));
}

#[test]
fn delegated_sponsorship_fails_before_runtime_mutation() {
    let plan = SwarmCompositionPlan {
        session_id: SessionId::new("session"),
        requested_by: AgentInstanceId::new("moderator"),
        members: vec![defined("worker", "someone-else")],
        relations: Vec::new(),
    };
    assert!(matches!(
        plan.ordered_members(),
        Err(RuntimeError::Coordination(message)) if message.contains("direct sponsor")
    ));
}
