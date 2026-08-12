//! Latest-only, UI-facing confirmation contract for governed memory.
//!
//! Requests never carry an owner selector. Runtime derives the stable user,
//! Agent, and session from the authenticated boundary and the persisted
//! session before it reads or changes a Guardian candidate.

use serde::{Deserialize, Serialize};

/// Negotiated capability for Guardian memory confirmation.
pub const MEMORY_CONFIRMATION_CAPABILITY: &str = "memory_confirmation_v1";
/// The only confirmation protocol revision accepted by this pre-release build.
pub const MEMORY_CONFIRMATION_PROTOCOL_VERSION: u16 = 1;

/// The governed destination described to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryConfirmationScope {
    Relationship,
    UserProfile,
    AgentCanonical,
    WorkspaceKnowledge,
}

/// An explicit user choice. Omitting a choice never implies consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryConfirmationDecision {
    Confirm,
    Reject,
}

/// Bounded, owner-safe prompt for one pending Guardian candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PendingMemoryConfirmation {
    pub candidate_id: String,
    pub expected_revision: u64,
    pub scope: MemoryConfirmationScope,
    /// Human-readable candidate text, bounded and sanitized by Runtime.
    pub summary: String,
}

/// Client operation. The authenticated boundary supplies all owner authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryConfirmationRequest {
    List {
        version: u16,
        session_id: String,
    },
    Decide {
        version: u16,
        session_id: String,
        candidate_id: String,
        expected_revision: u64,
        decision: MemoryConfirmationDecision,
    },
}

impl MemoryConfirmationRequest {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::List { .. } => "list_memory_confirmations",
            Self::Decide { .. } => "decide_memory_confirmation",
        }
    }

    pub fn session_id(&self) -> &str {
        match self {
            Self::List { session_id, .. } | Self::Decide { session_id, .. } => session_id,
        }
    }

    pub fn validate(&self) -> Result<(), MemoryConfirmationValidationError> {
        let version = match self {
            Self::List { version, .. } | Self::Decide { version, .. } => *version,
        };
        if version != MEMORY_CONFIRMATION_PROTOCOL_VERSION {
            return Err(MemoryConfirmationValidationError::UnsupportedVersion);
        }
        validate_id(self.session_id())?;
        if let Self::Decide {
            candidate_id,
            expected_revision,
            ..
        } = self
        {
            validate_id(candidate_id)?;
            if *expected_revision == 0 {
                return Err(MemoryConfirmationValidationError::InvalidRequest);
            }
        }
        Ok(())
    }
}

/// Content-safe terminal result for one confirmation operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryConfirmationResponse {
    Pending {
        version: u16,
        session_id: String,
        confirmations: Vec<PendingMemoryConfirmation>,
    },
    Recorded {
        version: u16,
        session_id: String,
        candidate_id: String,
        decision: MemoryConfirmationDecision,
    },
    Error {
        version: u16,
        operation: String,
        code: MemoryConfirmationErrorCode,
        message: String,
    },
}

impl MemoryConfirmationResponse {
    pub fn service_unavailable(operation: impl Into<String>) -> Self {
        Self::Error {
            version: MEMORY_CONFIRMATION_PROTOCOL_VERSION,
            operation: operation.into(),
            code: MemoryConfirmationErrorCode::ServiceUnavailable,
            message: "memory confirmation service is unavailable".into(),
        }
    }
}

/// Stable, non-reflective failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryConfirmationErrorCode {
    UnsupportedVersion,
    InvalidRequest,
    Unauthenticated,
    Forbidden,
    Conflict,
    ServiceUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryConfirmationValidationError {
    UnsupportedVersion,
    InvalidRequest,
}

fn validate_id(value: &str) -> Result<(), MemoryConfirmationValidationError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 256
        || value.chars().any(char::is_control)
    {
        return Err(MemoryConfirmationValidationError::InvalidRequest);
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/memory_confirmation.rs"]
mod tests;
