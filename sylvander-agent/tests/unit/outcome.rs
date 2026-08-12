use sylvander_llm_core::{ChatMessage, ModelRef, ModelResponse, StopReason, TokenUsage};

use crate::conversation::ConversationSnapshot;
use crate::outcome::AgentOutcome;

#[test]
fn outcome_returns_updated_conversation_to_runtime() {
    let response = ModelResponse {
        id: "response".into(),
        model: ModelRef::new("anthropic", "claude-sonnet-5-20260601"),
        content: Vec::new(),
        stop_reason: StopReason::EndTurn,
        usage: TokenUsage::default(),
    };
    let outcome = AgentOutcome {
        final_response: response.clone(),
        conversation: ConversationSnapshot::new(vec![ChatMessage::assistant(Vec::new())]),
        iterations: 1,
        total_usage: TokenUsage::default(),
    };

    assert_eq!(outcome.final_response, response);
    assert_eq!(outcome.conversation.messages().len(), 1);
}
