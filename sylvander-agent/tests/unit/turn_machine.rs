use sylvander_llm_core::{ChatMessage, ModelRef, ModelResponse, StopReason, TokenUsage};

use super::*;
use crate::turn::conversation::ConversationSnapshot;

fn response(stop_reason: StopReason) -> ModelResponse {
    ModelResponse {
        id: "response-1".into(),
        model: ModelRef::new("provider", "model"),
        content: Vec::new(),
        stop_reason,
        usage: TokenUsage {
            input_tokens: 3,
            output_tokens: 2,
            ..TokenUsage::default()
        },
    }
}

#[test]
fn legal_path_is_monotonic_and_snapshot_is_current_state() {
    let conversation = ConversationSnapshot::new(vec![ChatMessage::user("hello")]);
    let mut machine = TurnMachine::new(&conversation);
    let first = machine
        .transition(
            TurnPhase::Validating,
            TurnTransitionReason::ExecutionStarted,
        )
        .unwrap();
    assert_eq!(first.sequence, 1);
    machine
        .transition(
            TurnPhase::RunningBeforeHooks,
            TurnTransitionReason::RequestValidated,
        )
        .unwrap();
    machine
        .transition(
            TurnPhase::ReadyForIteration,
            TurnTransitionReason::BeforeHooksCompleted,
        )
        .unwrap();
    machine.start_iteration(1).unwrap();
    assert_eq!(
        machine.snapshot(),
        TurnSnapshot {
            sequence: 4,
            iteration: 1,
            phase: TurnPhase::PreparingIteration,
            continuation: None,
        }
    );
}

#[test]
fn invalid_and_post_terminal_transitions_fail_closed() {
    let mut machine = TurnMachine::new(&ConversationSnapshot::default());
    assert!(matches!(
        machine.transition(
            TurnPhase::ExecutingTools,
            TurnTransitionReason::ToolExecutionStarted
        ),
        Err(TurnStateError::InvalidTransition { .. })
    ));
    machine
        .transition(TurnPhase::Failed, TurnTransitionReason::ExecutionFailed)
        .unwrap();
    assert_eq!(
        machine.transition(
            TurnPhase::Validating,
            TurnTransitionReason::ExecutionStarted
        ),
        Err(TurnStateError::TerminalTransition)
    );
}

#[test]
fn continuation_and_usage_are_authoritative_machine_state() {
    let mut machine = TurnMachine::new(&ConversationSnapshot::default());
    machine.complete_iteration(
        response(StopReason::MaxOutputTokens),
        Some(TurnContinuationReason::MaxOutputTokens),
    );
    assert_eq!(machine.cumulative_usage().input_tokens, 3);
    assert_eq!(machine.cumulative_usage().output_tokens, 2);
    assert_eq!(
        machine.snapshot().continuation,
        Some(TurnContinuationReason::MaxOutputTokens)
    );
    assert_eq!(machine.outcome().unwrap().total_usage.input_tokens, 3);
}

#[test]
fn stable_projection_names_come_only_from_the_typed_vocabulary() {
    assert_eq!(
        TurnPhase::WaitingForApproval.as_str(),
        TurnPhase::WAITING_FOR_APPROVAL
    );
    assert_eq!(TurnPhase::Completed.as_str(), TurnPhase::COMPLETED);
    assert_eq!(
        TurnTransitionReason::ContinueAfterToolResults.as_str(),
        TurnTransitionReason::CONTINUE_AFTER_TOOL_RESULTS
    );
    assert_eq!(
        TurnContinuationReason::MaxOutputTokens.as_str(),
        TurnContinuationReason::MAX_OUTPUT_TOKENS
    );
}
