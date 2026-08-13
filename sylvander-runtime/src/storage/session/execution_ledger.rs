//! Durable tool execution positions and deterministic crash classification.
//!
//! The Session store persists these values; boot recovery derives one action
//! from them without consulting logs, public events, or capability audit.

use std::fmt;

use serde::{Deserialize, Serialize};
use sylvander_agent::tool::invocation::ToolRecoveryPolicy;
use uuid::Uuid;

/// Runtime-owned identity reused by authorization, execution, and recovery.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolInvocationId(String);

impl ToolInvocationId {
    /// Generate an opaque identity once, before approval or execution.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Reconstruct a previously validated durable identity.
    pub fn parse(value: String) -> Result<Self, uuid::Error> {
        Uuid::parse_str(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ToolInvocationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ToolInvocationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Last effect boundary durably crossed by one invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionPosition {
    Prepared,
    Authorized,
    EffectStarted,
    EffectCommitted,
    ResultPersisted,
}

impl ToolExecutionPosition {
    /// Whether a CAS may advance directly between two adjacent boundaries.
    #[must_use]
    pub const fn can_advance_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Prepared, Self::Authorized)
                | (Self::Authorized, Self::EffectStarted)
                | (Self::EffectStarted, Self::EffectCommitted)
                | (Self::EffectCommitted, Self::ResultPersisted)
        )
    }
}

/// Content-free action selected for one interrupted invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRecoveryDecision {
    ResumeAuthorization,
    StartEffect,
    RetrySameInvocation,
    Reconcile,
    RecoverResult,
    ContinueTurn,
    ManualReconciliation,
}

/// Stable reason for a recovery decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRecoveryReason {
    EffectNotStarted,
    SameIdentityReplayAllowed,
    ReconciliationRequired,
    ReconciliationConfirmedNoEffect,
    ReconciliationConfirmedRollback,
    ReconciliationUncertain,
    ReplayForbiddenAfterEffectStart,
    EffectAlreadyCommitted,
    ResultAlreadyPersisted,
}

/// Deterministic content-free result of classifying a durable position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryClassification {
    pub decision: ToolRecoveryDecision,
    pub reason: ToolRecoveryReason,
    pub operator_action_required: bool,
}

impl RecoveryClassification {
    /// Build a persisted classification from a trusted concrete adapter.
    #[must_use]
    pub const fn reconciled(
        decision: ToolRecoveryDecision,
        reason: ToolRecoveryReason,
        operator_action_required: bool,
    ) -> Self {
        Self {
            decision,
            reason,
            operator_action_required,
        }
    }

    /// Classify one non-terminal invocation from its frozen effective policy.
    #[must_use]
    pub const fn for_interrupted(
        position: ToolExecutionPosition,
        policy: ToolRecoveryPolicy,
    ) -> Self {
        let (decision, reason, operator_action_required) = match position {
            ToolExecutionPosition::Prepared => (
                ToolRecoveryDecision::ResumeAuthorization,
                ToolRecoveryReason::EffectNotStarted,
                false,
            ),
            ToolExecutionPosition::Authorized => (
                ToolRecoveryDecision::StartEffect,
                ToolRecoveryReason::EffectNotStarted,
                false,
            ),
            ToolExecutionPosition::EffectStarted => match policy {
                ToolRecoveryPolicy::NeverReplay => (
                    ToolRecoveryDecision::ManualReconciliation,
                    ToolRecoveryReason::ReplayForbiddenAfterEffectStart,
                    true,
                ),
                ToolRecoveryPolicy::RetryWithSameInvocation => (
                    ToolRecoveryDecision::RetrySameInvocation,
                    ToolRecoveryReason::SameIdentityReplayAllowed,
                    false,
                ),
                ToolRecoveryPolicy::ReconcileBeforeRetry => (
                    ToolRecoveryDecision::Reconcile,
                    ToolRecoveryReason::ReconciliationRequired,
                    false,
                ),
            },
            ToolExecutionPosition::EffectCommitted => (
                ToolRecoveryDecision::RecoverResult,
                ToolRecoveryReason::EffectAlreadyCommitted,
                false,
            ),
            ToolExecutionPosition::ResultPersisted => (
                ToolRecoveryDecision::ContinueTurn,
                ToolRecoveryReason::ResultAlreadyPersisted,
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

#[cfg(test)]
#[path = "../../../tests/unit/execution_ledger.rs"]
mod tests;
