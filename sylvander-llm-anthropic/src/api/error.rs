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
mod tests {
    use super::*;

    #[test]
    fn api_4xx_is_not_retryable() {
        let err = AnthropicError::Api {
            status: 400,
            error_type: "invalid_request_error".into(),
            error_message: "bad input".into(),
            request_id: Some("req_abc".into()),
        };
        assert!(!err.is_retryable());
        assert_eq!(err.status(), Some(400));
        assert_eq!(err.request_id(), Some("req_abc"));
    }

    #[test]
    fn api_429_is_retryable() {
        let err = AnthropicError::Api {
            status: 429,
            error_type: "rate_limit_error".into(),
            error_message: "slow down".into(),
            request_id: None,
        };
        assert!(err.is_retryable());
        assert_eq!(err.status(), Some(429));
    }

    #[test]
    fn api_5xx_is_retryable() {
        for status in [500u16, 502, 503, 504, 529] {
            let err = AnthropicError::Api {
                status,
                error_type: "api_error".into(),
                error_message: "transient".into(),
                request_id: None,
            };
            assert!(err.is_retryable(), "status {status} should be retryable");
            assert_eq!(err.status(), Some(status));
        }
    }

    #[test]
    fn json_error_is_not_retryable() {
        // Json error variant has no status
        let result: Result<serde_json::Value, _> = serde_json::from_str("{invalid");
        let err: AnthropicError = result.unwrap_err().into();
        assert!(!err.is_retryable());
        assert_eq!(err.status(), None);
        assert_eq!(err.request_id(), None);
    }

    #[test]
    fn sse_parse_is_retryable() {
        let err = AnthropicError::SseParse {
            message: "incomplete event".into(),
            position: 42,
        };
        assert!(err.is_retryable());
        assert_eq!(err.status(), None);
    }

    #[test]
    fn validation_error_is_not_retryable() {
        let err = AnthropicError::Validation("max_tokens must be > 0".into());
        assert!(!err.is_retryable());
        assert_eq!(err.status(), None);
    }

    #[test]
    fn unknown_block_type_is_not_retryable() {
        let err = AnthropicError::UnknownBlockType("weird_type".into());
        assert!(!err.is_retryable());
    }

    #[test]
    fn unknown_stream_event_type_is_not_retryable() {
        let err = AnthropicError::UnknownStreamEventType("mystery_event".into());
        assert!(!err.is_retryable());
    }

    #[test]
    fn display_messages_are_informative() {
        let err = AnthropicError::Api {
            status: 400,
            error_type: "invalid_request_error".into(),
            error_message: "model is required".into(),
            request_id: Some("req_xyz".into()),
        };
        let msg = format!("{err}");
        assert!(msg.contains("400"));
        assert!(msg.contains("invalid_request_error"));
        assert!(msg.contains("model is required"));
    }
}