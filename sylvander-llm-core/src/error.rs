//! Provider-neutral failure classification.

use thiserror::Error;

/// Stable provider-neutral failure category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// Network or client transport failed.
    Transport,
    /// The configured operation deadline elapsed.
    Timeout,
    /// The provider rejected work due to rate limits.
    RateLimited,
    /// Provider credentials were missing or invalid.
    Authentication,
    /// Credentials were valid but lacked permission.
    PermissionDenied,
    /// The requested model does not exist for this provider.
    ModelNotFound,
    /// The normalized request was invalid.
    InvalidRequest,
    /// The provider cannot implement a requested feature.
    Unsupported,
    /// The provider or model is temporarily unavailable.
    Unavailable,
    /// A response violated the provider wire contract.
    Protocol,
    /// The caller cancelled the operation.
    Cancelled,
    /// A provider-specific failure without a stronger classification.
    Other,
}

impl ProviderErrorKind {
    #[must_use]
    /// Return whether the Agent may consume retry budget for this category.
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Transport | Self::Timeout | Self::RateLimited | Self::Unavailable
        )
    }
}

/// Point in an invocation at which a provider failure occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorPhase {
    /// Before the event stream was successfully opened.
    Open,
    /// After the stream opened and while events were being consumed.
    Stream,
}

/// Content-safe normalized provider failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("provider {kind:?} error during {phase:?}: {message}")]
pub struct ProviderError {
    /// Stable failure category.
    pub kind: ProviderErrorKind,
    /// Invocation phase that failed.
    pub phase: ProviderErrorPhase,
    /// Content-safe diagnostic message.
    pub message: String,
    /// Optional HTTP-like provider status.
    pub status: Option<u16>,
    /// Optional provider request identifier.
    pub request_id: Option<String>,
    /// Optional retry delay advertised by the provider.
    pub retry_after_ms: Option<u64>,
}

impl ProviderError {
    #[must_use]
    /// Construct a normalized error without optional provider metadata.
    pub fn new(
        kind: ProviderErrorKind,
        phase: ProviderErrorPhase,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            phase,
            message: message.into(),
            status: None,
            request_id: None,
            retry_after_ms: None,
        }
    }

    #[must_use]
    /// Return whether retry policy may retry this failure.
    pub const fn is_retryable(&self) -> bool {
        self.kind.is_retryable()
    }
}
