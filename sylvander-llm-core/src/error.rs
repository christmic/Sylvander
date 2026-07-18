//! Provider-neutral failure classification.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    Transport,
    Timeout,
    RateLimited,
    Authentication,
    PermissionDenied,
    ModelNotFound,
    InvalidRequest,
    Unsupported,
    Unavailable,
    Protocol,
    Cancelled,
    Other,
}

impl ProviderErrorKind {
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Transport | Self::Timeout | Self::RateLimited | Self::Unavailable
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorPhase {
    Open,
    Stream,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("provider {kind:?} error during {phase:?}: {message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub phase: ProviderErrorPhase,
    pub message: String,
    pub status: Option<u16>,
    pub request_id: Option<String>,
    pub retry_after_ms: Option<u64>,
}

impl ProviderError {
    #[must_use]
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
    pub const fn is_retryable(&self) -> bool {
        self.kind.is_retryable()
    }
}
