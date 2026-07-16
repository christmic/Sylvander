use rusqlite::params;

use super::parse_status;
use crate::evidence::evaluation::{valid_key, valid_sha256};
use crate::evidence::proposal_types::{
    ImprovementProposalStatus, ProposalTransition, StoredImprovementProposal,
};
use crate::evidence::{EvidenceError, EvidenceStore, as_i64};

impl EvidenceStore {
    /// Advance a proposal through its explicit review/experiment lifecycle
    /// using optimistic concurrency and immutable actor attribution.
    pub async fn transition_improvement_proposal(
        &self,
        transition: ProposalTransition,
    ) -> Result<StoredImprovementProposal, EvidenceError> {
        validate_transition(&transition)?;
        let proposal_id = transition.proposal_id.clone();
        self.run(move |connection| {
            let transaction = connection
                .unchecked_transaction()
                .map_err(EvidenceError::sqlite)?;
            let (current_name, current_revision) = transaction
                .query_row(
                    "SELECT status, state_revision
                     FROM evidence_improvement_proposals WHERE id=?1",
                    [&transition.proposal_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => EvidenceError::ProposalStateConflict,
                    other => EvidenceError::sqlite(other),
                })?;
            let current = parse_status(&current_name)?;
            let current_revision =
                u64::try_from(current_revision).map_err(|_| EvidenceError::InvalidProposalData)?;
            if current_revision != transition.expected_state_revision
                || !allowed_transition(current, transition.status)
            {
                return Err(EvidenceError::ProposalStateConflict);
            }
            let next_revision = current_revision
                .checked_add(1)
                .ok_or(EvidenceError::CountTooLarge)?;
            let changed = transaction
                .execute(
                    "UPDATE evidence_improvement_proposals
                     SET status=?2, state_revision=?3
                     WHERE id=?1 AND state_revision=?4",
                    params![
                        transition.proposal_id,
                        status_name(transition.status),
                        as_i64(next_revision)?,
                        as_i64(current_revision)?
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            if changed != 1 {
                return Err(EvidenceError::ProposalStateConflict);
            }
            transaction
                .execute(
                    "INSERT INTO evidence_improvement_proposal_transitions(
                       proposal_id, state_revision, from_status, to_status,
                       principal_digest, reason, occurred_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        transition.proposal_id,
                        as_i64(next_revision)?,
                        status_name(current),
                        status_name(transition.status),
                        transition.principal_digest,
                        transition.reason,
                        transition.occurred_at
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            transaction.commit().map_err(EvidenceError::sqlite)
        })
        .await?;
        self.improvement_proposal(proposal_id)
            .await?
            .ok_or(EvidenceError::InvalidProposalData)
    }
}

fn validate_transition(transition: &ProposalTransition) -> Result<(), EvidenceError> {
    if !valid_key(&transition.proposal_id)
        || transition.expected_state_revision == 0
        || !valid_sha256(&transition.principal_digest)
        || transition
            .reason
            .as_ref()
            .is_some_and(|reason| reason.trim().is_empty() || reason.len() > 2048)
        || transition.occurred_at < 0
    {
        return Err(EvidenceError::InvalidImprovementProposal);
    }
    Ok(())
}

fn allowed_transition(current: ImprovementProposalStatus, next: ImprovementProposalStatus) -> bool {
    matches!(
        (current, next),
        (
            ImprovementProposalStatus::Draft,
            ImprovementProposalStatus::ReadyForReview
        ) | (
            ImprovementProposalStatus::ReadyForReview,
            ImprovementProposalStatus::Approved | ImprovementProposalStatus::Rejected
        ) | (
            ImprovementProposalStatus::Approved,
            ImprovementProposalStatus::Experimenting
        ) | (
            ImprovementProposalStatus::Experimenting,
            ImprovementProposalStatus::Completed | ImprovementProposalStatus::RolledBack
        )
    )
}

fn status_name(status: ImprovementProposalStatus) -> &'static str {
    match status {
        ImprovementProposalStatus::Draft => "draft",
        ImprovementProposalStatus::ReadyForReview => "ready_for_review",
        ImprovementProposalStatus::Approved => "approved",
        ImprovementProposalStatus::Rejected => "rejected",
        ImprovementProposalStatus::Experimenting => "experimenting",
        ImprovementProposalStatus::Completed => "completed",
        ImprovementProposalStatus::RolledBack => "rolled_back",
    }
}
