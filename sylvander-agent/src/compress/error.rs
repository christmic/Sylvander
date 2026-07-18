//! Stable internal classification for compaction failures.

use crate::error::AgentLoopError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionFailureCode {
    Busy,
    InsufficientHistory,
    Provider,
    Protocol,
    Persistence,
    UnsupportedBackend,
    SessionUnavailable,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct CompactionError {
    pub code: CompactionFailureCode,
    reason: &'static str,
}

impl CompactionError {
    #[must_use]
    pub const fn new(code: CompactionFailureCode) -> Self {
        let reason = match code {
            CompactionFailureCode::Busy => "interrupt active work before compacting",
            CompactionFailureCode::InsufficientHistory => {
                "not enough conversation history to compact"
            }
            CompactionFailureCode::Provider => "model provider request failed during compaction",
            CompactionFailureCode::Protocol => "model provider returned an invalid summary",
            CompactionFailureCode::Persistence => "compacted history could not be persisted",
            CompactionFailureCode::UnsupportedBackend => {
                "compaction is unavailable for this model backend"
            }
            CompactionFailureCode::SessionUnavailable => "session is unavailable for compaction",
            CompactionFailureCode::Other => "compaction failed",
        };
        Self { code, reason }
    }

    #[must_use]
    pub const fn compatibility_reason(&self) -> &'static str {
        self.reason
    }

    #[must_use]
    pub fn from_loop(error: &AgentLoopError) -> Self {
        match error {
            AgentLoopError::Provider { source, .. }
                if source.kind == sylvander_llm_core::ProviderErrorKind::Protocol =>
            {
                Self::new(CompactionFailureCode::Protocol)
            }
            AgentLoopError::Provider { .. } => Self::new(CompactionFailureCode::Provider),
            AgentLoopError::Compression(_) | AgentLoopError::Validation(_) => {
                Self::new(CompactionFailureCode::Protocol)
            }
            _ => Self::new(CompactionFailureCode::Other),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/compress_error.rs"]
mod tests;
