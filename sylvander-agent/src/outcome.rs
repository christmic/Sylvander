//! Terminal result of one Agent execution.
//!
//! The Agent returns computation results; Runtime decides whether and how to
//! commit them to a product Session. A successful value does not itself imply
//! that durable persistence or client publication has succeeded.

use sylvander_llm_core::{ModelResponse, TokenUsage};

use crate::conversation::ConversationSnapshot;

/// Provider-neutral result returned to Runtime for durable commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOutcome {
    /// Final normalized provider response.
    pub final_response: ModelResponse,
    /// Updated model-visible transcript for Runtime to commit atomically.
    pub conversation: ConversationSnapshot,
    /// Number of model/tool iterations completed.
    pub iterations: u32,
    /// Cumulative normalized token usage for the execution.
    pub total_usage: TokenUsage,
}

#[cfg(test)]
#[path = "../tests/unit/outcome.rs"]
mod tests;
