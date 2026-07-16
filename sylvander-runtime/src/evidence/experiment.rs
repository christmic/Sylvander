use rusqlite::{OptionalExtension, params};

use super::evaluation::{valid_key, valid_sha256};
use super::experiment_signer::valid_git_commit;
use super::experiment_types::{
    SelfChangeExperiment, SelfChangeExperimentStatus, StoredSelfChangeExperiment,
};
use super::proposal::parse_status as parse_proposal_status;
use super::proposal_types::ImprovementProposalStatus;
use super::{EvidenceError, EvidenceStore, as_i64};

mod evidence;
mod transition;

impl EvidenceStore {
    /// Atomically bind one approved proposal to one isolated worktree lease.
    pub async fn register_self_change_experiment(
        &self,
        definition: SelfChangeExperiment,
    ) -> Result<StoredSelfChangeExperiment, EvidenceError> {
        validate_definition(&definition)?;
        let experiment_id = definition.id.clone();
        self.run(move |connection| {
            let transaction = connection
                .unchecked_transaction()
                .map_err(EvidenceError::sqlite)?;
            let existing = transaction
                .query_row(
                    "SELECT proposal_id FROM evidence_self_change_experiments WHERE id=?1",
                    [&definition.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(EvidenceError::sqlite)?;
            if existing.is_some() {
                return Err(EvidenceError::ExperimentStateConflict);
            }
            let (proposal_status, proposal_revision) = transaction
                .query_row(
                    "SELECT status, state_revision FROM evidence_improvement_proposals
                     WHERE id=?1",
                    [&definition.proposal_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => {
                        EvidenceError::InvalidExperimentEvidence
                    }
                    other => EvidenceError::sqlite(other),
                })?;
            let proposal_status = parse_proposal_status(&proposal_status)?;
            let proposal_revision =
                u64::try_from(proposal_revision).map_err(|_| EvidenceError::InvalidProposalData)?;
            if proposal_status != ImprovementProposalStatus::Approved
                || proposal_revision != definition.proposal_state_revision
            {
                return Err(EvidenceError::ExperimentStateConflict);
            }
            transaction
                .execute(
                    "INSERT INTO evidence_self_change_experiments(
                       id, proposal_id, lease_id, branch, base_commit,
                       proposal_state_revision, started_by_principal_digest,
                       created_at, status, state_revision
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'prepared', 1)",
                    params![
                        definition.id,
                        definition.proposal_id,
                        definition.lease_id,
                        definition.branch,
                        definition.base_commit,
                        as_i64(definition.proposal_state_revision)?,
                        definition.started_by_principal_digest,
                        definition.created_at
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            let next_proposal_revision = proposal_revision
                .checked_add(1)
                .ok_or(EvidenceError::CountTooLarge)?;
            let changed = transaction
                .execute(
                    "UPDATE evidence_improvement_proposals
                     SET status='experimenting', state_revision=?2
                     WHERE id=?1 AND state_revision=?3",
                    params![
                        definition.proposal_id,
                        as_i64(next_proposal_revision)?,
                        as_i64(proposal_revision)?
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
                     ) VALUES (?1, ?2, 'approved', 'experimenting', ?3, ?4, ?5)",
                    params![
                        definition.proposal_id,
                        as_i64(next_proposal_revision)?,
                        definition.started_by_principal_digest,
                        "isolated self-change experiment started",
                        definition.created_at
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            transaction.commit().map_err(EvidenceError::sqlite)
        })
        .await?;
        self.self_change_experiment(experiment_id)
            .await?
            .ok_or(EvidenceError::InvalidExperimentData)
    }

    pub async fn self_change_experiment(
        &self,
        id: String,
    ) -> Result<Option<StoredSelfChangeExperiment>, EvidenceError> {
        self.run(move |connection| {
            connection
                .query_row(
                    "SELECT proposal_id, lease_id, branch, base_commit,
                            proposal_state_revision, started_by_principal_digest,
                            created_at, status, state_revision,
                            baseline_bundle_id, candidate_bundle_id, merge_commit,
                            rollback_commit, observation_bundle_id, merge_approved_by
                     FROM evidence_self_change_experiments WHERE id=?1",
                    [&id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, String>(7)?,
                            row.get::<_, i64>(8)?,
                            row.get::<_, Option<String>>(9)?,
                            row.get::<_, Option<String>>(10)?,
                            row.get::<_, Option<String>>(11)?,
                            row.get::<_, Option<String>>(12)?,
                            row.get::<_, Option<String>>(13)?,
                            row.get::<_, Option<String>>(14)?,
                        ))
                    },
                )
                .optional()
                .map_err(EvidenceError::sqlite)?
                .map(|row| {
                    Ok(StoredSelfChangeExperiment {
                        definition: SelfChangeExperiment {
                            id,
                            proposal_id: row.0,
                            lease_id: row.1,
                            branch: row.2,
                            base_commit: row.3,
                            proposal_state_revision: u64::try_from(row.4)
                                .map_err(|_| EvidenceError::InvalidExperimentData)?,
                            started_by_principal_digest: row.5,
                            created_at: row.6,
                        },
                        status: parse_experiment_status(&row.7)?,
                        state_revision: u64::try_from(row.8)
                            .map_err(|_| EvidenceError::InvalidExperimentData)?,
                        baseline_bundle_id: row.9,
                        candidate_bundle_id: row.10,
                        merge_commit: row.11,
                        rollback_commit: row.12,
                        observation_bundle_id: row.13,
                        merge_approved_by: row.14,
                    })
                })
                .transpose()
        })
        .await
    }
}

fn validate_definition(definition: &SelfChangeExperiment) -> Result<(), EvidenceError> {
    if !valid_key(&definition.id)
        || !valid_key(&definition.proposal_id)
        || !valid_key(&definition.lease_id)
        || definition.branch.is_empty()
        || definition.branch.len() > 256
        || definition
            .branch
            .bytes()
            .any(|byte| byte.is_ascii_control())
        || !valid_git_commit(&definition.base_commit)
        || definition.proposal_state_revision == 0
        || !valid_sha256(&definition.started_by_principal_digest)
        || definition.created_at < 0
    {
        return Err(EvidenceError::InvalidExperimentEvidence);
    }
    Ok(())
}

pub(super) fn parse_experiment_status(
    value: &str,
) -> Result<SelfChangeExperimentStatus, EvidenceError> {
    match value {
        "prepared" => Ok(SelfChangeExperimentStatus::Prepared),
        "candidate_evaluated" => Ok(SelfChangeExperimentStatus::CandidateEvaluated),
        "merge_approved" => Ok(SelfChangeExperimentStatus::MergeApproved),
        "merged" => Ok(SelfChangeExperimentStatus::Merged),
        "observing" => Ok(SelfChangeExperimentStatus::Observing),
        "completed" => Ok(SelfChangeExperimentStatus::Completed),
        "rollback_required" => Ok(SelfChangeExperimentStatus::RollbackRequired),
        "rolled_back" => Ok(SelfChangeExperimentStatus::RolledBack),
        "failed" => Ok(SelfChangeExperimentStatus::Failed),
        _ => Err(EvidenceError::InvalidExperimentData),
    }
}

pub(super) fn experiment_status_name(status: SelfChangeExperimentStatus) -> &'static str {
    match status {
        SelfChangeExperimentStatus::Prepared => "prepared",
        SelfChangeExperimentStatus::CandidateEvaluated => "candidate_evaluated",
        SelfChangeExperimentStatus::MergeApproved => "merge_approved",
        SelfChangeExperimentStatus::Merged => "merged",
        SelfChangeExperimentStatus::Observing => "observing",
        SelfChangeExperimentStatus::Completed => "completed",
        SelfChangeExperimentStatus::RollbackRequired => "rollback_required",
        SelfChangeExperimentStatus::RolledBack => "rolled_back",
        SelfChangeExperimentStatus::Failed => "failed",
    }
}
