//! Durable model-iteration positions and crash recovery classification.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identity assigned before one provider request may start.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelInvocationId(String);

impl ModelInvocationId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn parse(value: String) -> Result<Self, uuid::Error> {
        Uuid::parse_str(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ModelInvocationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ModelInvocationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Last durable boundary crossed by one model iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelExecutionPosition {
    ModelStarted,
    ResponsePersisted,
    ToolsResolved,
}

impl ModelExecutionPosition {
    #[must_use]
    pub const fn can_advance_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::ModelStarted, Self::ResponsePersisted)
                | (Self::ResponsePersisted, Self::ToolsResolved)
        )
    }
}

/// Content-free action derived exclusively from durable iteration facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRecoveryDecision {
    ManualReconciliation,
    RecoverTools,
    CompleteTurn,
    ContinueTurn,
    OperatorAbandoned,
}

/// Stable reason exposed to operators without leaking prompts or responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRecoveryReason {
    ProviderOutcomeUnknown,
    DurableToolResponse,
    DurableTerminalResponse,
    ToolsAlreadyResolved,
    IncompleteDurableFacts,
    OperatorAbandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelRecoveryClassification {
    pub decision: ModelRecoveryDecision,
    pub reason: ModelRecoveryReason,
    pub operator_action_required: bool,
}

impl ModelRecoveryClassification {
    /// Missing or contradictory response facts always fail closed.
    #[must_use]
    pub const fn for_interrupted(
        position: ModelExecutionPosition,
        response_message_id: Option<i64>,
        response_terminal: Option<bool>,
    ) -> Self {
        match (position, response_message_id, response_terminal) {
            (ModelExecutionPosition::ModelStarted, None, None) => Self {
                decision: ModelRecoveryDecision::ManualReconciliation,
                reason: ModelRecoveryReason::ProviderOutcomeUnknown,
                operator_action_required: true,
            },
            (ModelExecutionPosition::ResponsePersisted, Some(_), Some(false)) => Self {
                decision: ModelRecoveryDecision::RecoverTools,
                reason: ModelRecoveryReason::DurableToolResponse,
                operator_action_required: false,
            },
            (ModelExecutionPosition::ResponsePersisted, Some(_), Some(true)) => Self {
                decision: ModelRecoveryDecision::CompleteTurn,
                reason: ModelRecoveryReason::DurableTerminalResponse,
                operator_action_required: false,
            },
            (ModelExecutionPosition::ToolsResolved, Some(_), Some(_)) => Self {
                decision: ModelRecoveryDecision::ContinueTurn,
                reason: ModelRecoveryReason::ToolsAlreadyResolved,
                operator_action_required: false,
            },
            _ => Self {
                decision: ModelRecoveryDecision::ManualReconciliation,
                reason: ModelRecoveryReason::IncompleteDurableFacts,
                operator_action_required: true,
            },
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/model_ledger.rs"]
mod tests;
