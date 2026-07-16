use rusqlite::params;

use super::experiment_status_name;
use crate::evidence::evaluation::{valid_key, valid_sha256};
use crate::evidence::experiment_signer::valid_git_commit;
use crate::evidence::{
    EvidenceError, EvidenceStore, ExperimentTransition, SelfChangeExperimentStatus,
    StoredSelfChangeExperiment, as_i64,
};

impl EvidenceStore {
    /// Human approval is a distinct durable gate after candidate evaluation.
    pub async fn approve_experiment_merge(
        &self,
        transition: ExperimentTransition,
    ) -> Result<StoredSelfChangeExperiment, EvidenceError> {
        if transition
            .reason
            .as_ref()
            .is_none_or(|reason| reason.trim().is_empty())
        {
            return Err(EvidenceError::InvalidExperimentEvidence);
        }
        let approver = transition.principal_digest.clone();
        self.transition_experiment(
            transition,
            SelfChangeExperimentStatus::CandidateEvaluated,
            SelfChangeExperimentStatus::MergeApproved,
            Some(("merge_approved_by", approver)),
            None,
        )
        .await
    }

    pub async fn record_experiment_merge(
        &self,
        transition: ExperimentTransition,
        merge_commit: String,
    ) -> Result<StoredSelfChangeExperiment, EvidenceError> {
        if !valid_git_commit(&merge_commit) {
            return Err(EvidenceError::InvalidExperimentEvidence);
        }
        self.transition_experiment(
            transition,
            SelfChangeExperimentStatus::MergeApproved,
            SelfChangeExperimentStatus::Merged,
            Some(("merge_commit", merge_commit)),
            None,
        )
        .await
    }

    pub async fn begin_experiment_observation(
        &self,
        transition: ExperimentTransition,
    ) -> Result<StoredSelfChangeExperiment, EvidenceError> {
        self.transition_experiment(
            transition,
            SelfChangeExperimentStatus::Merged,
            SelfChangeExperimentStatus::Observing,
            None,
            None,
        )
        .await
    }

    pub async fn record_experiment_rollback(
        &self,
        transition: ExperimentTransition,
        rollback_commit: String,
    ) -> Result<StoredSelfChangeExperiment, EvidenceError> {
        if !valid_git_commit(&rollback_commit) {
            return Err(EvidenceError::InvalidExperimentEvidence);
        }
        self.transition_experiment(
            transition,
            SelfChangeExperimentStatus::RollbackRequired,
            SelfChangeExperimentStatus::RolledBack,
            Some(("rollback_commit", rollback_commit)),
            Some("rolled_back"),
        )
        .await
    }

    async fn transition_experiment(
        &self,
        transition: ExperimentTransition,
        expected_status: SelfChangeExperimentStatus,
        next_status: SelfChangeExperimentStatus,
        field: Option<(&'static str, String)>,
        finish_proposal_status: Option<&'static str>,
    ) -> Result<StoredSelfChangeExperiment, EvidenceError> {
        validate_transition(&transition)?;
        let experiment_id = transition.experiment_id.clone();
        self.run(move |connection| {
            let transaction = connection
                .unchecked_transaction()
                .map_err(EvidenceError::sqlite)?;
            let (proposal_id, current_status, current_revision) = transaction
                .query_row(
                    "SELECT proposal_id, status, state_revision
                     FROM evidence_self_change_experiments WHERE id=?1",
                    [&transition.experiment_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .map_err(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => EvidenceError::ExperimentStateConflict,
                    other => EvidenceError::sqlite(other),
                })?;
            let current_revision = u64::try_from(current_revision)
                .map_err(|_| EvidenceError::InvalidExperimentData)?;
            if current_status != experiment_status_name(expected_status)
                || current_revision != transition.expected_state_revision
            {
                return Err(EvidenceError::ExperimentStateConflict);
            }
            let next_revision = current_revision
                .checked_add(1)
                .ok_or(EvidenceError::CountTooLarge)?;
            let changed = if let Some((column, value)) = field {
                let update = format!(
                    "UPDATE evidence_self_change_experiments
                     SET status=?2, state_revision=?3, {column}=?4
                     WHERE id=?1 AND state_revision=?5"
                );
                transaction
                    .execute(
                        &update,
                        params![
                            transition.experiment_id,
                            experiment_status_name(next_status),
                            as_i64(next_revision)?,
                            value,
                            as_i64(current_revision)?
                        ],
                    )
                    .map_err(EvidenceError::sqlite)?
            } else {
                transaction
                    .execute(
                        "UPDATE evidence_self_change_experiments
                         SET status=?2, state_revision=?3
                         WHERE id=?1 AND state_revision=?4",
                        params![
                            transition.experiment_id,
                            experiment_status_name(next_status),
                            as_i64(next_revision)?,
                            as_i64(current_revision)?
                        ],
                    )
                    .map_err(EvidenceError::sqlite)?
            };
            if changed != 1 {
                return Err(EvidenceError::ExperimentStateConflict);
            }
            transaction
                .execute(
                    "INSERT INTO evidence_experiment_transitions(
                       experiment_id, state_revision, from_status, to_status,
                       principal_digest, reason, occurred_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        transition.experiment_id,
                        as_i64(next_revision)?,
                        experiment_status_name(expected_status),
                        experiment_status_name(next_status),
                        transition.principal_digest,
                        transition.reason,
                        transition.occurred_at
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            if let Some(status) = finish_proposal_status {
                finish_proposal_in_transaction(
                    &transaction,
                    &proposal_id,
                    status,
                    &transition.principal_digest,
                    transition.reason.as_deref(),
                    transition.occurred_at,
                )?;
            }
            transaction.commit().map_err(EvidenceError::sqlite)
        })
        .await?;
        self.self_change_experiment(experiment_id)
            .await?
            .ok_or(EvidenceError::InvalidExperimentData)
    }
}

pub(super) fn finish_proposal_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    proposal_id: &str,
    next_status: &'static str,
    principal_digest: &str,
    reason: Option<&str>,
    occurred_at: i64,
) -> Result<(), EvidenceError> {
    let revision = transaction
        .query_row(
            "SELECT state_revision FROM evidence_improvement_proposals
             WHERE id=?1 AND status='experimenting'",
            [proposal_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => EvidenceError::ProposalStateConflict,
            other => EvidenceError::sqlite(other),
        })?;
    let next_revision = revision
        .checked_add(1)
        .ok_or(EvidenceError::CountTooLarge)?;
    let changed = transaction
        .execute(
            "UPDATE evidence_improvement_proposals
             SET status=?2, state_revision=?3 WHERE id=?1 AND state_revision=?4",
            params![proposal_id, next_status, next_revision, revision],
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
             ) VALUES (?1, ?2, 'experimenting', ?3, ?4, ?5, ?6)",
            params![
                proposal_id,
                next_revision,
                next_status,
                principal_digest,
                reason,
                occurred_at
            ],
        )
        .map_err(EvidenceError::sqlite)?;
    Ok(())
}

fn validate_transition(transition: &ExperimentTransition) -> Result<(), EvidenceError> {
    if !valid_key(&transition.experiment_id)
        || transition.expected_state_revision == 0
        || !valid_sha256(&transition.principal_digest)
        || transition
            .reason
            .as_ref()
            .is_some_and(|reason| reason.trim().is_empty() || reason.len() > 2048)
        || transition.occurred_at < 0
    {
        return Err(EvidenceError::InvalidExperimentEvidence);
    }
    Ok(())
}
