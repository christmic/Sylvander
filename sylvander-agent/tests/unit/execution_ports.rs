use std::sync::Arc;

use sylvander_llm_core::{
    ChatMessage, ModelCapabilities, ModelEventStream, ModelInfo, ModelProvider, ModelRef,
    ProviderFuture,
};

use crate::conversation::ConversationSnapshot;
use crate::execution_context::AgentExecutionContext;
use crate::execution_ports::AgentExecutionPorts;
use crate::request::AgentTurnRequest;
use crate::tool::ToolRegistry;
use crate::tool_context::ToolContext;
use crate::tool_invocation::RegistryBoundToolGateway;

struct EmptyModelProvider;

impl ModelProvider for EmptyModelProvider {
    fn complete_stream(&self, _request: sylvander_llm_core::ModelRequest) -> ProviderFuture<'_> {
        Box::pin(async {
            let stream: ModelEventStream = Box::pin(futures_util::stream::empty());
            Ok(stream)
        })
    }
}

fn request_fixture() -> AgentTurnRequest {
    AgentTurnRequest {
        conversation: ConversationSnapshot::new(vec![ChatMessage::user("hello")]),
        model: ModelInfo {
            reference: ModelRef::new("test", "model"),
            context_window: 8_192,
            max_output_tokens: 1_024,
            capabilities: ModelCapabilities::TOOL_USE,
        },
        system_instructions: Vec::new(),
        reasoning: None,
        tools: ToolRegistry::new(),
        execution: AgentExecutionContext::restricted_for("user", "agent", "session"),
    }
}

fn ports(execution: AgentExecutionContext, tools: &ToolRegistry) -> AgentExecutionPorts {
    let model: Arc<dyn ModelProvider> = Arc::new(EmptyModelProvider);
    AgentExecutionPorts::new(
        model,
        ToolContext::new(execution),
        RegistryBoundToolGateway::new(tools.invocation_descriptors()),
    )
}

#[test]
fn matching_request_and_ports_are_accepted() {
    let request = request_fixture();
    let ports = ports(request.execution.clone(), &request.tools);

    assert!(ports.validate_for(&request).is_ok());
}

#[test]
fn mismatched_execution_authority_is_rejected() {
    let request = request_fixture();
    let ports = ports(
        AgentExecutionContext::restricted_for("other-user", "agent", "session"),
        &request.tools,
    );

    let error = ports.validate_for(&request).unwrap_err();
    assert!(error.to_string().contains("different authority"));
}

#[allow(dead_code)]
fn request_type_remains_domain_data(request: AgentTurnRequest) -> ConversationSnapshot {
    request.conversation
}
