//! Boot-time classification of interrupted Agent tool executions.
//!
//! This coordinator persists decisions only. Automated replay and Agent
//! continuation remain disabled until their concrete adapters are composed.

use std::sync::Arc;

use uuid::Uuid;

use crate::observability::{RuntimeEvent, RuntimeObservability};
use crate::storage::session::{
    RecoveryClassification, SessionStore, SessionStoreError, ToolRecoveryDecision,
    ToolRecoveryWrite,
};

const RECOVERY_LEASE_SECS: i64 = 30;

/// Content-free outcome of one complete boot classification pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BootRecoverySummary {
    pub discovered: u64,
    pub classified: u64,
    pub manual_reconciliation: u64,
}

/// Classify every interrupted call before persistent Sessions are attached.
///
/// Any storage or lease conflict fails boot closed. This function never calls
/// an execution adapter and therefore cannot replay an external effect.
pub(crate) async fn classify_interrupted_tool_calls(
    store: Arc<dyn SessionStore>,
    observability: &RuntimeObservability,
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
    for call in calls {
        let classification =
            RecoveryClassification::for_interrupted(call.position, call.effective_recovery_policy);
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
