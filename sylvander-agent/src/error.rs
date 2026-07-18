//! Agent loop error type.

use sylvander_llm_core::ProviderError;
use thiserror::Error;

/// Errors returned by the agent loop.
#[derive(Debug, Error)]
pub enum AgentLoopError {
    /// The agent ran for `max_iterations` without reaching
    /// `stop_reason: "end_turn"`. Usually means the model is stuck in a
    /// `tool_use` loop or the iteration limit is too low.
    #[error("max iterations ({0}) reached without end_turn")]
    MaxIterationsReached(u32),

    /// The configured model lacks a capability required by the request
    /// (e.g., tools set but model lacks `TOOL_USE`, `cache_control` with
    /// a TTL the model doesn't support).
    #[error("incompatible model: {0}")]
    IncompatibleModel(String),

    /// Provider-neutral model invocation failure.
    #[error("model provider error after {attempts} attempts: {source}")]
    Provider {
        /// Total provider requests made, including the initial request.
        attempts: u32,
        #[source]
        source: ProviderError,
    },

    /// Tool execution failed in a non-recoverable way (panic caught,
    /// unrecoverable runtime error, etc.). Note: tools that return
    /// `is_error: true` from `Tool::execute` are NOT this — those
    /// flow through the loop normally as a model-visible error.
    #[error("tool execution failed: {0}")]
    Tool(String),

    /// Compression strategy failed. The Compressor implementation
    /// reported an error.
    #[error("compression error: {0}")]
    Compression(String),

    /// Validation error (e.g., missing model, no messages, invalid
    /// request shape).
    #[error("validation error: {0}")]
    Validation(String),

    /// Builder was incomplete (required field not set).
    #[error("builder error: {0}")]
    Builder(String),
}

impl AgentLoopError {
    /// `true` if the underlying error is transient and the agent
    /// should consider retrying from a fresh state (e.g., re-issue
    /// the last LLM call). Note: retry WITHIN a single call is
    /// handled by `call_with_retry` in A7 — this is for callers
    /// that want to retry the entire loop from scratch.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Provider { source, .. } => source.is_retryable(),
            Self::MaxIterationsReached(_)
            | Self::IncompatibleModel(_)
            | Self::Tool(_)
            | Self::Compression(_)
            | Self::Validation(_)
            | Self::Builder(_) => false,
        }
    }

    /// HTTP status code if this error came from an API response.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Provider { source, .. } => source.status,
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/error.rs"]
mod tests;
