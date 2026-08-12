//! Immutable input for one Agent execution.
//!
//! Runtime resolves every field before calling the Agent. Keeping the complete
//! input in one value prevents model, tool, workspace, and authority snapshots
//! from drifting independently during a turn. This is an internal execution
//! contract, not an API request and therefore has no Serde implementation.

use sylvander_llm_core::{ModelInfo, ReasoningConfig, SystemInstruction};

use crate::conversation::ConversationSnapshot;
use crate::execution_context::AgentExecutionContext;
use crate::tool::ToolRegistry;

/// Complete turn input constructed by Runtime after Session authorization.
#[derive(Clone)]
pub struct AgentTurnRequest {
    /// Exact model-visible transcript selected for this execution.
    pub conversation: ConversationSnapshot,
    /// Provider-qualified model metadata pinned by Runtime.
    pub model: ModelInfo,
    /// Ordered provider-neutral system instructions.
    pub system_instructions: Vec<SystemInstruction>,
    /// Optional provider-neutral reasoning request.
    pub reasoning: Option<ReasoningConfig>,
    /// Immutable executable tool registry selected for this turn.
    pub tools: ToolRegistry,
    /// Trusted non-wire execution identity and authority.
    pub execution: AgentExecutionContext,
}

impl std::fmt::Debug for AgentTurnRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentTurnRequest")
            .field("message_count", &self.conversation.messages().len())
            .field("model", &self.model.reference)
            .field("system_instruction_count", &self.system_instructions.len())
            .field("reasoning", &self.reasoning)
            .field("tools", &self.tools)
            .field("execution", &self.execution)
            .finish()
    }
}

#[cfg(test)]
#[path = "../tests/unit/request.rs"]
mod tests;
