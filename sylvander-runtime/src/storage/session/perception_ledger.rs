//! Durable execution positions for built-in Agent perception.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PerceptionInvocationId(String);

impl PerceptionInvocationId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, PerceptionLedgerError> {
        let value = value.into();
        Uuid::parse_str(&value).map_err(|_| PerceptionLedgerError::InvalidInvocation)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for PerceptionInvocationId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerceptionExecutionPosition {
    Prepared,
    MediaPersisted,
    InferenceStarted,
    InferenceCompleted,
    ArtifactPersisted,
    ResultPersisted,
}

impl PerceptionExecutionPosition {
    #[must_use]
    pub const fn can_advance_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Prepared, Self::MediaPersisted)
                | (Self::MediaPersisted, Self::InferenceStarted)
                | (Self::InferenceStarted, Self::InferenceCompleted)
                | (Self::InferenceCompleted, Self::ArtifactPersisted)
                | (Self::ArtifactPersisted, Self::ResultPersisted)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerceptionRecoveryPolicy {
    NeverReplay,
    RetryWithSameInvocation,
    RecoverFromReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerceptionRecoveryDecision {
    PersistMedia,
    StartInference,
    RetrySameInvocation,
    RecoverReceipt,
    PersistArtifact,
    ContinueTurn,
    ManualReconciliation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerceptionRecoveryReason {
    EffectNotStarted,
    SameIdentityReplayAllowed,
    ReceiptRequired,
    InferenceOutcomeUncertain,
    ReceiptAlreadyPersisted,
    ArtifactAlreadyPersisted,
    ResultAlreadyPersisted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerceptionRecoveryClassification {
    pub decision: PerceptionRecoveryDecision,
    pub reason: PerceptionRecoveryReason,
    pub operator_action_required: bool,
}

impl PerceptionRecoveryClassification {
    #[must_use]
    pub const fn for_interrupted(
        position: PerceptionExecutionPosition,
        policy: PerceptionRecoveryPolicy,
    ) -> Self {
        let (decision, reason, operator_action_required) = match position {
            PerceptionExecutionPosition::Prepared => (
                PerceptionRecoveryDecision::PersistMedia,
                PerceptionRecoveryReason::EffectNotStarted,
                false,
            ),
            PerceptionExecutionPosition::MediaPersisted => (
                PerceptionRecoveryDecision::StartInference,
                PerceptionRecoveryReason::EffectNotStarted,
                false,
            ),
            PerceptionExecutionPosition::InferenceStarted => match policy {
                PerceptionRecoveryPolicy::NeverReplay => (
                    PerceptionRecoveryDecision::ManualReconciliation,
                    PerceptionRecoveryReason::InferenceOutcomeUncertain,
                    true,
                ),
                PerceptionRecoveryPolicy::RetryWithSameInvocation => (
                    PerceptionRecoveryDecision::RetrySameInvocation,
                    PerceptionRecoveryReason::SameIdentityReplayAllowed,
                    false,
                ),
                PerceptionRecoveryPolicy::RecoverFromReceipt => (
                    PerceptionRecoveryDecision::RecoverReceipt,
                    PerceptionRecoveryReason::ReceiptRequired,
                    false,
                ),
            },
            PerceptionExecutionPosition::InferenceCompleted => (
                PerceptionRecoveryDecision::PersistArtifact,
                PerceptionRecoveryReason::ReceiptAlreadyPersisted,
                false,
            ),
            PerceptionExecutionPosition::ArtifactPersisted => (
                PerceptionRecoveryDecision::ContinueTurn,
                PerceptionRecoveryReason::ArtifactAlreadyPersisted,
                false,
            ),
            PerceptionExecutionPosition::ResultPersisted => (
                PerceptionRecoveryDecision::ContinueTurn,
                PerceptionRecoveryReason::ResultAlreadyPersisted,
                false,
            ),
        };
        Self {
            decision,
            reason,
            operator_action_required,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PerceptionLedgerError {
    #[error("perception invocation identity is invalid")]
    InvalidInvocation,
}

#[cfg(test)]
#[path = "../../../tests/unit/perception_ledger.rs"]
mod tests;
