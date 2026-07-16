//! Agent loop error type.

use sylvander_llm_anthropic::api::error::AnthropicError;
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

    /// LLM call failed after the configured number of retries. The
    /// inner error preserves the last Anthropic SDK error.
    #[error("LLM error after {retries} retries: {source}")]
    Llm {
        /// Number of retry attempts made (0 = first try, no retries).
        retries: u32,
        /// Last error from the LLM SDK.
        #[source]
        source: AnthropicError,
    },

    /// Provider-neutral model invocation failure. New provider-backed paths
    /// use this variant while the legacy Anthropic path remains compatible.
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
    /// request shape). Surfaces the underlying `AnthropicError`.
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
            Self::Llm { source, .. } => source.is_retryable(),
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
            Self::Llm { source, .. } => source.status(),
            Self::Provider { source, .. } => source.status,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylvander_llm_anthropic::api::error::AnthropicError;
    use sylvander_llm_core::{ProviderErrorKind, ProviderErrorPhase};

    #[test]
    fn max_iterations_display() {
        let err = AgentLoopError::MaxIterationsReached(50);
        assert!(format!("{err}").contains("50"));
        assert!(!err.is_retryable());
        assert_eq!(err.status(), None);
    }

    #[test]
    fn incompatible_model_display() {
        let err = AgentLoopError::IncompatibleModel("model lacks TOOL_USE".into());
        assert!(!err.is_retryable());
    }

    #[test]
    fn llm_4xx_not_retryable() {
        let inner = AnthropicError::Api {
            status: 400,
            error_type: "invalid_request_error".into(),
            error_message: "bad input".into(),
            request_id: None,
        };
        let err = AgentLoopError::Llm {
            retries: 0,
            source: inner,
        };
        assert!(!err.is_retryable());
        assert_eq!(err.status(), Some(400));
    }

    #[test]
    fn llm_429_is_retryable() {
        let inner = AnthropicError::Api {
            status: 429,
            error_type: "rate_limit_error".into(),
            error_message: "slow down".into(),
            request_id: None,
        };
        let err = AgentLoopError::Llm {
            retries: 2,
            source: inner,
        };
        assert!(err.is_retryable());
        assert_eq!(err.status(), Some(429));
        assert!(format!("{err}").contains("2 retries"));
    }

    #[test]
    fn llm_5xx_is_retryable() {
        let inner = AnthropicError::Api {
            status: 503,
            error_type: "api_error".into(),
            error_message: "overloaded".into(),
            request_id: None,
        };
        let err = AgentLoopError::Llm {
            retries: 3,
            source: inner,
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn provider_retryability_and_status_are_typed() {
        let mut source = ProviderError::new(
            ProviderErrorKind::RateLimited,
            ProviderErrorPhase::Open,
            "model provider rate limit reached",
        );
        source.status = Some(429);
        let err = AgentLoopError::Provider {
            attempts: 2,
            source,
        };
        assert!(err.is_retryable());
        assert_eq!(err.status(), Some(429));
        assert!(format!("{err}").contains("2 attempts"));

        let err = AgentLoopError::Provider {
            attempts: 1,
            source: ProviderError::new(
                ProviderErrorKind::Authentication,
                ProviderErrorPhase::Open,
                "model provider authentication failed",
            ),
        };
        assert!(!err.is_retryable());
        assert_eq!(err.status(), None);
    }

    #[test]
    fn tool_error_not_retryable() {
        let err = AgentLoopError::Tool("panic in user tool".into());
        assert!(!err.is_retryable());
    }

    #[test]
    fn compression_error_not_retryable() {
        let err = AgentLoopError::Compression("invalid threshold".into());
        assert!(!err.is_retryable());
    }

    #[test]
    fn validation_error_not_retryable() {
        let err = AgentLoopError::Validation("messages empty".into());
        assert!(!err.is_retryable());
    }

    #[test]
    fn builder_error_not_retryable() {
        let err = AgentLoopError::Builder("missing client".into());
        assert!(!err.is_retryable());
    }
}
