//! Durable execution positions for text cognition inside one Agent turn.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CognitionInvocationId(String);

impl CognitionInvocationId {
    #[must_use]
    pub fn from_uuid(value: Uuid) -> Self {
        Self(value.to_string())
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, CognitionLedgerError> {
        let value = value.into();
        Uuid::parse_str(&value).map_err(|_| CognitionLedgerError::InvalidInvocation)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitionExecutionPosition {
    Prepared,
    PromptPersisted,
    InferenceStarted,
    InferenceCompleted,
    ArtifactPersisted,
    ResultPersisted,
    Failed,
}

impl CognitionExecutionPosition {
    #[must_use]
    pub const fn can_advance_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Prepared, Self::PromptPersisted)
                | (Self::PromptPersisted, Self::InferenceStarted)
                | (Self::InferenceStarted, Self::InferenceCompleted)
                | (Self::InferenceCompleted, Self::ArtifactPersisted)
                | (Self::ArtifactPersisted, Self::ResultPersisted)
        )
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::ResultPersisted | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitionRecoveryPolicy {
    RecoverFromReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitionFailureKind {
    Provider,
    TimedOut,
    InvalidResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitionRecoveryDecision {
    PersistPrompt,
    StartInference,
    RecoverReceipt,
    PersistArtifact,
    ContinueTurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitionRecoveryReason {
    EffectNotStarted,
    ReceiptRequired,
    ReceiptAlreadyPersisted,
    ArtifactAlreadyPersisted,
    ResultAlreadyPersisted,
    TerminalFailurePersisted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CognitionRecoveryClassification {
    pub decision: CognitionRecoveryDecision,
    pub reason: CognitionRecoveryReason,
    pub operator_action_required: bool,
}

impl CognitionRecoveryClassification {
    #[must_use]
    pub const fn for_interrupted(position: CognitionExecutionPosition) -> Self {
        let (decision, reason) = match position {
            CognitionExecutionPosition::Prepared => (
                CognitionRecoveryDecision::PersistPrompt,
                CognitionRecoveryReason::EffectNotStarted,
            ),
            CognitionExecutionPosition::PromptPersisted => (
                CognitionRecoveryDecision::StartInference,
                CognitionRecoveryReason::EffectNotStarted,
            ),
            CognitionExecutionPosition::InferenceStarted => (
                CognitionRecoveryDecision::RecoverReceipt,
                CognitionRecoveryReason::ReceiptRequired,
            ),
            CognitionExecutionPosition::InferenceCompleted => (
                CognitionRecoveryDecision::PersistArtifact,
                CognitionRecoveryReason::ReceiptAlreadyPersisted,
            ),
            CognitionExecutionPosition::ArtifactPersisted => (
                CognitionRecoveryDecision::ContinueTurn,
                CognitionRecoveryReason::ArtifactAlreadyPersisted,
            ),
            CognitionExecutionPosition::ResultPersisted => (
                CognitionRecoveryDecision::ContinueTurn,
                CognitionRecoveryReason::ResultAlreadyPersisted,
            ),
            CognitionExecutionPosition::Failed => (
                CognitionRecoveryDecision::ContinueTurn,
                CognitionRecoveryReason::TerminalFailurePersisted,
            ),
        };
        Self {
            decision,
            reason,
            operator_action_required: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CognitionLedgerError {
    #[error("cognition invocation identity is invalid")]
    InvalidInvocation,
}

#[cfg(test)]
mod tests {
    #[test]
    fn positions_are_adjacent_and_receipt_recovery_never_blindly_replays() {
        assert!(
            super::CognitionExecutionPosition::Prepared
                .can_advance_to(super::CognitionExecutionPosition::PromptPersisted)
        );
        assert!(
            !super::CognitionExecutionPosition::PromptPersisted
                .can_advance_to(super::CognitionExecutionPosition::InferenceCompleted)
        );
        assert_eq!(
            super::CognitionRecoveryClassification::for_interrupted(
                super::CognitionExecutionPosition::InferenceStarted
            )
            .decision,
            super::CognitionRecoveryDecision::RecoverReceipt
        );
    }
}
