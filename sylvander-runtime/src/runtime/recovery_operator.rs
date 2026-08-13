use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Runtime, RuntimeError};
use crate::agent::run::AuthenticatedSession;
use crate::storage::session::{
    ExecutionRecoveryAction, ExecutionRecoveryActionId, ExecutionRecoveryActionReceipt,
    ExecutionRecoveryActionTarget, ExecutionRecoveryActionWrite, ModelExecutionPosition,
    ModelInvocationId, ModelRecoveryReason, ToolExecutionPosition, ToolInvocationId,
    ToolRecoveryReason, TurnState,
};

const RECOVERY_ACTION_LEASE_SECONDS: i64 = 30;
const MAX_RECOVERY_RATIONALE_BYTES: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionRecoveryCase {
    Model {
        turn_id: String,
        invocation_id: ModelInvocationId,
        ledger_revision: u64,
        position: ModelExecutionPosition,
        reason: ModelRecoveryReason,
        actions: Vec<ExecutionRecoveryAction>,
    },
    Tool {
        turn_id: String,
        call_id: String,
        invocation_id: ToolInvocationId,
        ledger_revision: u64,
        position: ToolExecutionPosition,
        reason: ToolRecoveryReason,
        actions: Vec<ExecutionRecoveryAction>,
    },
}

impl SessionRecoveryCase {
    fn target(&self) -> ExecutionRecoveryActionTarget {
        match self {
            Self::Model { invocation_id, .. } => ExecutionRecoveryActionTarget::Model {
                invocation_id: invocation_id.clone(),
            },
            Self::Tool { invocation_id, .. } => ExecutionRecoveryActionTarget::Tool {
                invocation_id: invocation_id.clone(),
            },
        }
    }

    fn turn_id(&self) -> &str {
        match self {
            Self::Model { turn_id, .. } | Self::Tool { turn_id, .. } => turn_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveSessionRecoveryRequest {
    pub action_id: ExecutionRecoveryActionId,
    pub turn_id: String,
    pub target: ExecutionRecoveryActionTarget,
    pub expected_ledger_revision: u64,
    pub action: ExecutionRecoveryAction,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveSessionRecoveryOutcome {
    pub receipt: ExecutionRecoveryActionReceipt,
    pub replayed_tool_calls: u64,
    pub session_released: bool,
}

impl Runtime {
    pub async fn session_recovery_cases(
        &self,
        actor: &AuthenticatedSession,
    ) -> Result<Vec<SessionRecoveryCase>, RuntimeError> {
        self.require_recovery_moderator(actor).await?;
        let session_id = actor.id();
        let turns = self.storage.sessions();
        let models = turns
            .interrupted_model_iterations()
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?;
        let tools = turns
            .interrupted_tool_calls()
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?;
        let mut cases = Vec::new();
        for model in models
            .into_iter()
            .filter(|model| &model.session_id == session_id && model.operator_action_required)
        {
            let Some(reason) = model.recovery_reason else {
                return Err(RuntimeError::Store(
                    "manual model recovery has no durable reason".into(),
                ));
            };
            cases.push(SessionRecoveryCase::Model {
                turn_id: model.turn_id,
                invocation_id: model.invocation_id,
                ledger_revision: model.ledger_revision,
                position: model.position,
                reason,
                actions: vec![ExecutionRecoveryAction::AbandonTurn],
            });
        }
        for tool in tools
            .into_iter()
            .filter(|tool| &tool.session_id == session_id && tool.operator_action_required)
        {
            let Some(reason) = tool.recovery_reason else {
                return Err(RuntimeError::Store(
                    "manual tool recovery has no durable reason".into(),
                ));
            };
            let running_turn = turns
                .turn(session_id, &tool.turn_id)
                .await
                .map_err(|error| RuntimeError::Store(error.to_string()))?
                .is_some_and(|turn| turn.state == TurnState::Running);
            let mut actions = vec![ExecutionRecoveryAction::AbandonTurn];
            if running_turn && tool.position == ToolExecutionPosition::EffectStarted {
                actions.push(ExecutionRecoveryAction::ConfirmNoEffectAndRetry);
            }
            cases.push(SessionRecoveryCase::Tool {
                turn_id: tool.turn_id,
                call_id: tool.call_id,
                invocation_id: tool.invocation_id,
                ledger_revision: tool.ledger_revision,
                position: tool.position,
                reason,
                actions,
            });
        }
        cases.sort_by(|left, right| {
            left.turn_id().cmp(right.turn_id()).then_with(|| {
                left.target()
                    .invocation_id()
                    .cmp(right.target().invocation_id())
            })
        });
        Ok(cases)
    }

    pub async fn resolve_session_recovery(
        &self,
        actor: &AuthenticatedSession,
        request: ResolveSessionRecoveryRequest,
    ) -> Result<ResolveSessionRecoveryOutcome, RuntimeError> {
        self.require_recovery_moderator(actor).await?;
        let rationale = request.rationale.trim();
        if rationale.is_empty()
            || rationale.len() > MAX_RECOVERY_RATIONALE_BYTES
            || rationale.chars().any(char::is_control)
        {
            return Err(RuntimeError::Coordination(
                "recovery rationale is invalid".into(),
            ));
        }
        if request.turn_id.trim().is_empty() {
            return Err(RuntimeError::Coordination(
                "recovery turn identity is invalid".into(),
            ));
        }
        let now = crate::session::now_secs();
        let lease_expires_at = now
            .checked_add(RECOVERY_ACTION_LEASE_SECONDS)
            .ok_or_else(|| RuntimeError::Coordination("recovery lease time overflow".into()))?;
        let receipt = self
            .storage
            .sessions()
            .resolve_execution_recovery(ExecutionRecoveryActionWrite {
                action_id: request.action_id,
                session_id: actor.id().clone(),
                turn_id: request.turn_id,
                target: request.target,
                expected_ledger_revision: request.expected_ledger_revision,
                action: request.action,
                resolved_by: actor.agent_instance_id().clone(),
                rationale_digest: format!("sha256:{:x}", Sha256::digest(rationale.as_bytes())),
                observed_at: now,
                lease_expires_at,
            })
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?;
        self.record_coordination(
            &receipt.session_id,
            match receipt.action {
                ExecutionRecoveryAction::AbandonTurn => {
                    crate::observability::RuntimeCoordinationOutcome::RecoveryAbandoned
                }
                ExecutionRecoveryAction::ConfirmNoEffectAndRetry => {
                    crate::observability::RuntimeCoordinationOutcome::RecoveryRetryAuthorized
                }
            },
        );
        let mut replayed_tool_calls = 0;
        if receipt.action == ExecutionRecoveryAction::ConfirmNoEffectAndRetry {
            let turn = self
                .storage
                .sessions()
                .turn(&receipt.session_id, &receipt.turn_id)
                .await
                .map_err(|error| RuntimeError::Store(error.to_string()))?
                .ok_or_else(|| RuntimeError::Coordination("recovery turn disappeared".into()))?;
            let membership = self
                .storage
                .sessions()
                .session_membership(&receipt.session_id)
                .await
                .map_err(|error| RuntimeError::Store(error.to_string()))?
                .ok_or_else(|| RuntimeError::Coordination("Agent membership disappeared".into()))?;
            let participant = membership
                .participants
                .iter()
                .find(|participant| participant.instance_id == turn.agent_instance_id)
                .ok_or_else(|| RuntimeError::Coordination("recovery Agent disappeared".into()))?;
            let configured = self
                .configured_agent_revision(
                    &participant.definition.agent_id,
                    participant.definition.revision,
                )
                .await?;
            replayed_tool_calls = configured
                .run
                .replay_classified_tool_calls(
                    &receipt.session_id,
                    &turn.agent_instance_id,
                    receipt.action_id.as_str(),
                    now,
                )
                .await
                .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        }
        if replayed_tool_calls != 0 {
            let turn = self
                .storage
                .sessions()
                .turn(&receipt.session_id, &receipt.turn_id)
                .await
                .map_err(|error| RuntimeError::Store(error.to_string()))?
                .ok_or_else(|| RuntimeError::Coordination("recovery turn disappeared".into()))?;
            if turn.state == TurnState::Running {
                self.storage
                    .sessions()
                    .finish_turn(
                        &receipt.session_id,
                        &receipt.turn_id,
                        TurnState::Interrupted,
                        None,
                    )
                    .await
                    .map_err(|error| RuntimeError::Store(error.to_string()))?;
            }
        }
        let session_released = self
            .storage
            .sessions()
            .turn(&receipt.session_id, &receipt.turn_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .is_some_and(|turn| turn.state != TurnState::Running);
        Ok(ResolveSessionRecoveryOutcome {
            receipt,
            replayed_tool_calls,
            session_released,
        })
    }

    async fn require_recovery_moderator(
        &self,
        actor: &AuthenticatedSession,
    ) -> Result<(), RuntimeError> {
        let membership = self
            .storage
            .sessions()
            .session_membership(actor.id())
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .ok_or_else(|| RuntimeError::Coordination("Agent membership does not exist".into()))?;
        super::validate_coordination_actor(
            actor,
            actor.id(),
            &membership.governance.moderator_instance_id,
        )
    }
}
