//! Boot-time classification of interrupted Agent tool executions.
//!
//! This coordinator persists decisions only. Automated replay and Agent
//! continuation remain disabled until their concrete adapters are composed.

use std::sync::Arc;

use uuid::Uuid;

use crate::observability::{RuntimeEvent, RuntimeObservability};
use crate::storage::session::{
    RecoveryClassification, SessionStore, SessionStoreError, ToolCallAdvance,
    ToolExecutionPosition, ToolRecoveryDecision, ToolRecoveryReason, ToolRecoveryWrite,
};
use crate::storage::workspace_journal::{WorkspaceJournal, WorkspaceMutationRecovery};

const RECOVERY_LEASE_SECS: i64 = 30;

/// Content-free outcome of one complete boot classification pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BootRecoverySummary {
    pub discovered: u64,
    pub classified: u64,
    pub lease_deferred: u64,
    pub manual_reconciliation: u64,
}

/// Classify every interrupted call before persistent Sessions are attached.
///
/// Any storage or lease conflict fails boot closed. This function never calls
/// an execution adapter and therefore cannot replay an external effect.
pub(crate) async fn classify_interrupted_tool_calls(
    store: Arc<dyn SessionStore>,
    observability: &RuntimeObservability,
    workspace_journal: Option<&WorkspaceJournal>,
    observed_at: i64,
) -> Result<BootRecoverySummary, SessionStoreError> {
    let calls = store.interrupted_tool_calls().await?;
    let mut summary = BootRecoverySummary {
        discovered: calls.len() as u64,
        ..BootRecoverySummary::default()
    };
    if calls.is_empty() {
        return Ok(summary);
    }
    let owner = format!("boot-recovery:{}", Uuid::new_v4());
    let lease_expires_at = observed_at
        .checked_add(RECOVERY_LEASE_SECS)
        .ok_or_else(|| SessionStoreError::Invalid("tool recovery lease time overflow".into()))?;
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
        let mut classification =
            RecoveryClassification::for_interrupted(call.position, call.effective_recovery_policy);
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

fn active_lease_decision(
    decision: Option<ToolRecoveryDecision>,
    lease_expires_at: Option<i64>,
    observed_at: i64,
) -> Option<ToolRecoveryDecision> {
    lease_expires_at
        .is_some_and(|expires_at| expires_at > observed_at)
        .then_some(decision)
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::active_lease_decision;
    use crate::storage::session::ToolRecoveryDecision;

    #[test]
    fn active_classification_lease_is_observed_without_reacquisition() {
        assert_eq!(
            active_lease_decision(
                Some(ToolRecoveryDecision::ManualReconciliation),
                Some(130),
                100,
            ),
            Some(ToolRecoveryDecision::ManualReconciliation)
        );
        assert_eq!(
            active_lease_decision(
                Some(ToolRecoveryDecision::RetrySameInvocation),
                Some(100),
                100,
            ),
            None
        );
        assert_eq!(active_lease_decision(None, Some(130), 100), None);
    }
}
