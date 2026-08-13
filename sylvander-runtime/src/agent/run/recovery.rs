//! Boot-time recovery of interrupted Agent execution ledgers.
//!
//! Decisions are persisted before action. A terminal response can be finalized
//! without replaying a provider or tool; all uncertain effects fail closed.

use std::sync::Arc;

use uuid::Uuid;

use crate::observability::{RuntimeEvent, RuntimeObservability};
use crate::storage::session::{
    CognitionRecoveryClassification, CognitionRecoveryWrite, ModelRecoveryClassification,
    ModelRecoveryDecision, ModelRecoveryWrite, PerceptionRecoveryClassification,
    PerceptionRecoveryWrite, PersistedTurnCompletion, RecoveryClassification, SessionStore,
    SessionStoreError, ToolCallAdvance, ToolExecutionPosition, ToolRecoveryDecision,
    ToolRecoveryReason, ToolRecoveryWrite,
};
use crate::storage::workspace_journal::{WorkspaceJournal, WorkspaceMutationRecovery};

const RECOVERY_LEASE_SECS: i64 = 30;

/// Content-free outcome of one complete boot classification pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BootRecoverySummary {
    pub discovered: u64,
    pub classified: u64,
    pub lease_deferred: u64,
    pub manual_reconciliation: u64,
    pub model_discovered: u64,
    pub model_classified: u64,
    pub perception_discovered: u64,
    pub perception_classified: u64,
    pub cognition_discovered: u64,
    pub cognition_classified: u64,
    pub turns_completed: u64,
    pub recovery_owner: Option<String>,
}

/// Classify every interrupted call before persistent Sessions are attached.
///
/// Any storage or lease conflict fails boot closed. This function never
/// replays a provider or external tool effect.
pub(crate) async fn classify_interrupted_tool_calls(
    store: Arc<dyn SessionStore>,
    observability: &RuntimeObservability,
    workspace_journal: Option<&WorkspaceJournal>,
    observed_at: i64,
) -> Result<BootRecoverySummary, SessionStoreError> {
    let owner = format!("boot-recovery:{}", Uuid::new_v4());
    let lease_expires_at = observed_at
        .checked_add(RECOVERY_LEASE_SECS)
        .ok_or_else(|| SessionStoreError::Invalid("recovery lease time overflow".into()))?;
    let mut summary = recover_interrupted_model_iterations(
        store.clone(),
        observability,
        &owner,
        observed_at,
        lease_expires_at,
    )
    .await?;
    summary.recovery_owner = Some(owner.clone());
    recover_interrupted_perceptions(
        store.clone(),
        observability,
        &owner,
        observed_at,
        lease_expires_at,
        &mut summary,
    )
    .await?;
    recover_interrupted_cognition(
        store.clone(),
        &owner,
        observed_at,
        lease_expires_at,
        &mut summary,
    )
    .await?;
    let calls = store.interrupted_tool_calls().await?;
    summary.discovered = calls.len() as u64;
    if calls.is_empty() {
        return Ok(summary);
    }
    for mut call in calls {
        if let Some(decision) = active_lease_decision(
            call.recovery_decision,
            call.recovery_lease_expires_at,
            observed_at,
        ) {
            summary.lease_deferred = summary.lease_deferred.saturating_add(1);
            if decision == ToolRecoveryDecision::ManualReconciliation {
                summary.manual_reconciliation = summary.manual_reconciliation.saturating_add(1);
            }
            observability.record(RuntimeEvent::ToolRecoveryClassified {
                turn_id: call.turn_id,
                session_id: call.session_id,
                tool_call_id: call.call_id,
                position: call.position,
                decision,
                operator_action_required: call.operator_action_required,
            });
            continue;
        }
        let mut classification = if call.recovery_decision
            == Some(ToolRecoveryDecision::RetrySameInvocation)
            && call.recovery_reason == Some(ToolRecoveryReason::OperatorConfirmedNoEffect)
        {
            // The moderator established a durable external fact. Once its
            // previous lease expires, preserve that fact while transferring
            // replay ownership to this recovery pass; never fall back to the
            // tool's conservative default policy and lose the authorization.
            RecoveryClassification::reconciled(
                ToolRecoveryDecision::RetrySameInvocation,
                ToolRecoveryReason::OperatorConfirmedNoEffect,
                false,
            )
        } else {
            RecoveryClassification::for_interrupted(call.position, call.effective_recovery_policy)
        };
        if classification.decision == ToolRecoveryDecision::Reconcile {
            let recovery =
                workspace_journal.map_or(WorkspaceMutationRecovery::Unknown, |journal| {
                    journal.reconcile_tool_call(&call.session_id.0, &call.turn_id, &call.call_id)
                });
            match recovery {
                WorkspaceMutationRecovery::Committed => {
                    call.ledger_revision = store
                        .advance_tool_call(ToolCallAdvance {
                            session_id: call.session_id.clone(),
                            turn_id: call.turn_id.clone(),
                            call_id: call.call_id.clone(),
                            expected_revision: call.ledger_revision,
                            expected_position: ToolExecutionPosition::EffectStarted,
                            next_position: ToolExecutionPosition::EffectCommitted,
                        })
                        .await?;
                    call.position = ToolExecutionPosition::EffectCommitted;
                    classification = RecoveryClassification::for_interrupted(
                        call.position,
                        call.effective_recovery_policy,
                    );
                }
                WorkspaceMutationRecovery::NotCommitted => {
                    classification = RecoveryClassification::reconciled(
                        ToolRecoveryDecision::RetrySameInvocation,
                        ToolRecoveryReason::ReconciliationConfirmedNoEffect,
                        false,
                    );
                }
                WorkspaceMutationRecovery::RolledBack => {
                    classification = RecoveryClassification::reconciled(
                        ToolRecoveryDecision::RetrySameInvocation,
                        ToolRecoveryReason::ReconciliationConfirmedRollback,
                        false,
                    );
                }
                WorkspaceMutationRecovery::Unknown => {
                    classification = RecoveryClassification::reconciled(
                        ToolRecoveryDecision::ManualReconciliation,
                        ToolRecoveryReason::ReconciliationUncertain,
                        true,
                    );
                }
            }
        }
        store
            .classify_tool_recovery(ToolRecoveryWrite {
                invocation_id: call.invocation_id,
                expected_revision: call.ledger_revision,
                recovery_owner: owner.clone(),
                observed_at,
                lease_expires_at,
                classification,
            })
            .await?;
        summary.classified = summary.classified.saturating_add(1);
        if classification.decision == ToolRecoveryDecision::ManualReconciliation {
            summary.manual_reconciliation = summary.manual_reconciliation.saturating_add(1);
        }
        observability.record(RuntimeEvent::ToolRecoveryClassified {
            turn_id: call.turn_id,
            session_id: call.session_id,
            tool_call_id: call.call_id,
            position: call.position,
            decision: classification.decision,
            operator_action_required: classification.operator_action_required,
        });
    }
    Ok(summary)
}

async fn recover_interrupted_cognition(
    store: Arc<dyn SessionStore>,
    owner: &str,
    observed_at: i64,
    lease_expires_at: i64,
    summary: &mut BootRecoverySummary,
) -> Result<(), SessionStoreError> {
    let invocations = store.interrupted_cognition_invocations().await?;
    summary.cognition_discovered = invocations.len() as u64;
    for invocation in invocations {
        if active_lease_decision(
            invocation.recovery_decision,
            invocation.recovery_lease_expires_at,
            observed_at,
        )
        .is_some()
        {
            summary.lease_deferred = summary.lease_deferred.saturating_add(1);
            continue;
        }
        let classification = CognitionRecoveryClassification::for_interrupted(invocation.position);
        store
            .classify_cognition_recovery(CognitionRecoveryWrite {
                invocation_id: invocation.invocation_id,
                expected_revision: invocation.ledger_revision,
                recovery_owner: owner.to_owned(),
                observed_at,
                lease_expires_at,
                classification,
            })
            .await?;
        summary.cognition_classified = summary.cognition_classified.saturating_add(1);
    }
    Ok(())
}

async fn recover_interrupted_perceptions(
    store: Arc<dyn SessionStore>,
    observability: &RuntimeObservability,
    owner: &str,
    observed_at: i64,
    lease_expires_at: i64,
    summary: &mut BootRecoverySummary,
) -> Result<(), SessionStoreError> {
    let invocations = store.interrupted_perception_invocations().await?;
    summary.perception_discovered = invocations.len() as u64;
    for invocation in invocations {
        if let Some(decision) = active_lease_decision(
            invocation.recovery_decision,
            invocation.recovery_lease_expires_at,
            observed_at,
        ) {
            summary.lease_deferred = summary.lease_deferred.saturating_add(1);
            if invocation.operator_action_required {
                summary.manual_reconciliation = summary.manual_reconciliation.saturating_add(1);
            }
            observability.record(RuntimeEvent::PerceptionRecoveryClassified {
                turn_id: invocation.turn_id,
                session_id: invocation.session_id,
                invocation_id: invocation.invocation_id.as_str().to_owned(),
                position: invocation.position,
                decision,
                operator_action_required: invocation.operator_action_required,
            });
            continue;
        }
        let classification = PerceptionRecoveryClassification::for_interrupted(
            invocation.position,
            invocation.recovery_policy,
        );
        store
            .classify_perception_recovery(PerceptionRecoveryWrite {
                invocation_id: invocation.invocation_id.clone(),
                expected_revision: invocation.ledger_revision,
                recovery_owner: owner.to_owned(),
                observed_at,
                lease_expires_at,
                classification,
            })
            .await?;
        summary.perception_classified = summary.perception_classified.saturating_add(1);
        if classification.operator_action_required {
            summary.manual_reconciliation = summary.manual_reconciliation.saturating_add(1);
        }
        observability.record(RuntimeEvent::PerceptionRecoveryClassified {
            turn_id: invocation.turn_id,
            session_id: invocation.session_id,
            invocation_id: invocation.invocation_id.as_str().to_owned(),
            position: invocation.position,
            decision: classification.decision,
            operator_action_required: classification.operator_action_required,
        });
    }
    Ok(())
}

async fn recover_interrupted_model_iterations(
    store: Arc<dyn SessionStore>,
    observability: &RuntimeObservability,
    owner: &str,
    observed_at: i64,
    lease_expires_at: i64,
) -> Result<BootRecoverySummary, SessionStoreError> {
    let iterations = store.interrupted_model_iterations().await?;
    let mut summary = BootRecoverySummary {
        model_discovered: iterations.len() as u64,
        ..BootRecoverySummary::default()
    };
    for iteration in iterations {
        if active_lease_decision(
            iteration.recovery_decision,
            iteration.recovery_lease_expires_at,
            observed_at,
        )
        .is_some()
        {
            summary.lease_deferred = summary.lease_deferred.saturating_add(1);
            if iteration.operator_action_required {
                summary.manual_reconciliation = summary.manual_reconciliation.saturating_add(1);
            }
            continue;
        }
        let classification = ModelRecoveryClassification::for_interrupted(
            iteration.position,
            iteration.response_message_id,
            iteration.response_terminal,
        );
        let revision = store
            .classify_model_recovery(ModelRecoveryWrite {
                invocation_id: iteration.invocation_id.clone(),
                expected_revision: iteration.ledger_revision,
                recovery_owner: owner.to_owned(),
                observed_at,
                lease_expires_at,
                classification,
            })
            .await?;
        summary.model_classified = summary.model_classified.saturating_add(1);
        if classification.operator_action_required {
            summary.manual_reconciliation = summary.manual_reconciliation.saturating_add(1);
        }
        observability.record(RuntimeEvent::ModelRecoveryClassified {
            turn_id: iteration.turn_id,
            session_id: iteration.session_id,
            position: iteration.position,
            decision: classification.decision,
            operator_action_required: classification.operator_action_required,
        });
        if classification.decision == ModelRecoveryDecision::CompleteTurn {
            store
                .complete_persisted_turn(PersistedTurnCompletion {
                    invocation_id: iteration.invocation_id,
                    expected_revision: revision,
                })
                .await?;
            summary.turns_completed = summary.turns_completed.saturating_add(1);
        }
    }
    Ok(summary)
}

fn active_lease_decision<T: Copy>(
    decision: Option<T>,
    lease_expires_at: Option<i64>,
    observed_at: i64,
) -> Option<T> {
    lease_expires_at
        .is_some_and(|expires_at| expires_at > observed_at)
        .then_some(decision)
        .flatten()
}

#[cfg(test)]
#[path = "../../../tests/unit/agent_run_recovery.rs"]
mod tests;
