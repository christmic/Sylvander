use sylvander_llm_core::{
    ChatMessage, ModelCapabilities, ModelInfo, ModelRef, ReasoningConfig, ReasoningEffort,
    SystemInstruction,
};

use crate::conversation::ConversationSnapshot;
use crate::execution_context::{AgentExecutionContext, ExecutionActor};
use crate::request::AgentTurnRequest;
use crate::tool::ToolRegistry;

fn request_fixture() -> AgentTurnRequest {
    AgentTurnRequest {
        conversation: ConversationSnapshot::new(vec![ChatMessage::user("hello")]),
        model: ModelInfo {
            reference: ModelRef::new("openai", "gpt-5.6"),
            context_window: 200_000,
            max_output_tokens: 32_000,
            capabilities: ModelCapabilities::TOOL_USE,
        },
        system_instructions: vec![SystemInstruction {
            text: "help".into(),
            cache_hint: None,
        }],
        reasoning: Some(ReasoningConfig {
            budget_tokens: None,
            effort: Some(ReasoningEffort::High),
        }),
        tools: ToolRegistry::new(),
        execution: AgentExecutionContext::restricted(ExecutionActor::new("u", "a", "s")),
    }
}

#[test]
fn request_is_provider_neutral_and_contains_no_product_session_record() {
    let request = request_fixture();

    let cloned = request.clone();
    assert_eq!(cloned.model.reference, ModelRef::new("openai", "gpt-5.6"));
    assert_eq!(cloned.conversation.messages().len(), 1);
}
