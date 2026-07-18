//! Typed errors for the Anthropic Protocol SDK.
//!
//! Errors are classified into:
//!
//! | Variant | Retryable? | When |
//! |---------|------------|------|
//! | [`AnthropicError::Http`] | depends on inner reqwest error | network / transport |
//! | [`AnthropicError::Json`] | no | malformed JSON in our request body |
//! | [`AnthropicError::SseParse`] | no | malformed SSE in stream |
//! | [`AnthropicError::Api`] | status-dependent | Anthropic API error response |
//! | [`AnthropicError::UnknownBlockType`] | no | unknown `type` in content block |
//! | [`AnthropicError::UnknownStreamEventType`] | no | unknown SSE event `type` |
//! | [`AnthropicError::Validation`] | no | pre-flight request validation |
//!
//! 4xx responses are permanent (don't retry); 5xx + 429 are transient
//! (retry with backoff).

use thiserror::Error;

/// Errors from the Anthropic Protocol SDK.
#[derive(Debug, Error)]
pub enum AnthropicError {
    /// HTTP / transport error (network failure, DNS, TLS, timeout).
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// SSE (Server-Sent Events) parse error. Carries a human-readable
    /// message and the byte offset where parsing failed.
    #[error("SSE parse error at byte {position}: {message}")]
    SseParse {
        /// Description of the parse failure.
        message: String,
        /// Byte offset in the stream where the error occurred.
        position: usize,
    },

    /// Anthropic API error response. The error body is parsed into
    /// `error_type`, `error_message`, and `request_id` where available.
    #[error("Anthropic API error (status {status}): {error_type}: {error_message}")]
    Api {
        /// HTTP status code (e.g. 400, 401, 429, 500).
        status: u16,
        /// Anthropic's `type` field from the error body (e.g.
        /// `"invalid_request_error"`).
        error_type: String,
        /// Human-readable error message from Anthropic.
        error_message: String,
        /// Anthropic request ID — pass to support when reporting issues.
        request_id: Option<String>,
    },

    /// Encountered a content block with an unrecognized `type` field.
    #[error("unknown content block type: {0}")]
    UnknownBlockType(String),

    /// Encountered an SSE stream event with an unrecognized `type` field.
    #[error("unknown stream event type: {0}")]
    UnknownStreamEventType(String),

    /// Pre-flight validation error (caught before sending the request).
    #[error("validation error: {0}")]
    Validation(String),
}

impl AnthropicError {
    /// `true` if this error is transient and the request can be retried.
    ///
    /// Returns `true` for:
    /// - HTTP transport errors (network blips)
    /// - API errors with status 429 (rate limit) or 5xx (server error)
    /// - SSE parse errors mid-stream (network truncation)
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http(_) | Self::SseParse { .. } => true,
            Self::Api { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }

    /// HTTP status code, if this error came from an API response.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Anthropic request ID, if available. Pass this to Anthropic support
    /// when reporting an issue.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Api { request_id, .. } => request_id.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/api_error.rs"]
mod tests;
