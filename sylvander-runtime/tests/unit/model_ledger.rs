use super::*;

#[test]
fn unknown_provider_outcome_never_replays_without_a_response_fact() {
    let classification = ModelRecoveryClassification::for_interrupted(
        ModelExecutionPosition::ModelStarted,
        None,
        None,
    );

    assert_eq!(
        classification.decision,
        ModelRecoveryDecision::ManualReconciliation
    );
    assert_eq!(
        classification.reason,
        ModelRecoveryReason::ProviderOutcomeUnknown
    );
    assert!(classification.operator_action_required);
}

#[test]
fn incomplete_or_contradictory_facts_fail_closed() {
    for (message_id, terminal) in [(Some(7), None), (None, Some(false)), (None, Some(true))] {
        let classification = ModelRecoveryClassification::for_interrupted(
            ModelExecutionPosition::ResponsePersisted,
            message_id,
            terminal,
        );
        assert_eq!(
            classification.decision,
            ModelRecoveryDecision::ManualReconciliation
        );
        assert_eq!(
            classification.reason,
            ModelRecoveryReason::IncompleteDurableFacts
        );
        assert!(classification.operator_action_required);
    }
}

#[test]
fn complete_durable_facts_select_continuation_without_model_replay() {
    let tools = ModelRecoveryClassification::for_interrupted(
        ModelExecutionPosition::ResponsePersisted,
        Some(7),
        Some(false),
    );
    assert_eq!(tools.decision, ModelRecoveryDecision::RecoverTools);

    let terminal = ModelRecoveryClassification::for_interrupted(
        ModelExecutionPosition::ResponsePersisted,
        Some(8),
        Some(true),
    );
    assert_eq!(terminal.decision, ModelRecoveryDecision::CompleteTurn);

    let resolved = ModelRecoveryClassification::for_interrupted(
        ModelExecutionPosition::ToolsResolved,
        Some(7),
        Some(false),
    );
    assert_eq!(resolved.decision, ModelRecoveryDecision::ContinueTurn);
}
