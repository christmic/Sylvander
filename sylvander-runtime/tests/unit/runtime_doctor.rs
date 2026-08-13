use super::*;

fn empty_recovery() -> SessionRecoverySummary {
    SessionRecoverySummary {
        interrupted_models: 0,
        interrupted_perceptions: 0,
        interrupted_tools: 0,
        operator_models: 0,
        operator_perceptions: 0,
        operator_tools: 0,
    }
}

#[test]
fn perception_recovery_changes_doctor_attention_without_content_access() {
    let agents = SessionAgentSummary {
        total: 0,
        active: 0,
        waiting: 0,
        terminal: 0,
        manual_reconciliation: 0,
    };
    let tasks = SessionTaskSummary {
        total: 0,
        ready: 0,
        running: 0,
        blocked: 0,
        awaiting_review: 0,
        terminal: 0,
        remaining_token_budget: 0,
    };
    let workspaces = SessionWorkspaceSummary {
        active: 0,
        integrating: 0,
        conflicted: 0,
        manual_reconciliation: 0,
    };
    let governance = SessionGovernanceSummary {
        topology_relations: 0,
        open_arbitrations: 0,
    };
    let mut recovery = empty_recovery();
    recovery.interrupted_perceptions = 1;
    assert_eq!(
        attention(&agents, &tasks, &workspaces, &recovery, &governance),
        SessionAttentionState::Recovering
    );
    recovery.operator_perceptions = 1;
    assert_eq!(
        attention(&agents, &tasks, &workspaces, &recovery, &governance),
        SessionAttentionState::ManualActionRequired
    );
}
