use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params};

use super::experiment_status_name;
use super::transition::finish_proposal_in_transaction;
use crate::evidence::evaluation::{valid_key, valid_sha256};
use crate::evidence::{
    EvidenceError, EvidenceStore, ExperimentEvidenceSigner, ExperimentPhase, MetricMeasurement,
    RecordExperimentEvidence, SelfChangeExperimentStatus, SignedExperimentEvidence, as_i64,
    sign_experiment_evidence,
};

impl EvidenceStore {
    /// Recompute, sign, and durably attach one phase of experiment evidence.
    pub async fn record_experiment_evidence(
        &self,
        request: RecordExperimentEvidence,
        signer: &dyn ExperimentEvidenceSigner,
    ) -> Result<SignedExperimentEvidence, EvidenceError> {
        validate_request(&request)?;
        let experiment = self
            .self_change_experiment(request.evidence.experiment_id.clone())
            .await?
            .ok_or(EvidenceError::InvalidExperimentEvidence)?;
        if experiment.state_revision != request.expected_state_revision {
            return Err(EvidenceError::ExperimentStateConflict);
        }
        let proposal = self
            .improvement_proposal(experiment.definition.proposal_id.clone())
            .await?
            .ok_or(EvidenceError::InvalidExperimentEvidence)?;
        if request.evidence.proposal_digest_sha256 != proposal.digest_sha256 {
            return Err(EvidenceError::InvalidExperimentEvidence);
        }
        validate_phase_commit(&request, &experiment)?;

        let expected_baselines = proposal
            .definition
            .required_evaluations
            .iter()
            .map(|required| required.baseline_id.as_str())
            .collect::<BTreeSet<_>>();
        let actual_baselines = request
            .evidence
            .evaluations
            .iter()
            .map(|comparison| comparison.baseline_id.as_str())
            .collect::<BTreeSet<_>>();
        if expected_baselines != actual_baselines
            || actual_baselines.len() != request.evidence.evaluations.len()
        {
            return Err(EvidenceError::InvalidExperimentEvidence);
        }
        for comparison in &request.evidence.evaluations {
            let measurements = comparison
                .decisions
                .iter()
                .map(|decision| MetricMeasurement {
                    metric: decision.metric.clone(),
                    value: decision.candidate_value,
                    sample_count: decision.sample_count,
                })
                .collect();
            let recomputed = self
                .compare_evaluation_baseline(comparison.baseline_id.clone(), measurements)
                .await?;
            if recomputed != *comparison {
                return Err(EvidenceError::InvalidExperimentEvidence);
            }
        }

        let passed = request
            .evidence
            .evaluations
            .iter()
            .all(|comparison| comparison.passed);
        let (expected_status, passed_status) = phase_transition(request.evidence.phase);
        if experiment.status != expected_status {
            return Err(EvidenceError::ExperimentStateConflict);
        }
        let next_status = if passed {
            passed_status
        } else {
            failed_status(request.evidence.phase)
        };
        let (signed_bundle, canonical_json) =
            sign_experiment_evidence(request.id.clone(), request.evidence.clone(), signer)?;
        let signed_for_store = signed_bundle.clone();
        let principal_digest = request.principal_digest;
        let expected_revision = request.expected_state_revision;
        let recorded_at = request.evidence.recorded_at;
        self.run(move |connection| {
            let transaction = connection
                .unchecked_transaction()
                .map_err(EvidenceError::sqlite)?;
            let (proposal_id, current_status, current_revision) = transaction
                .query_row(
                    "SELECT proposal_id, status, state_revision
                     FROM evidence_self_change_experiments WHERE id=?1",
                    [&signed_for_store.evidence.experiment_id],
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
            if current_status != experiment_status_name(expected_status)
                || u64::try_from(current_revision)
                    .map_err(|_| EvidenceError::InvalidExperimentData)?
                    != expected_revision
            {
                return Err(EvidenceError::ExperimentStateConflict);
            }
            let existing = transaction
                .query_row(
                    "SELECT id FROM evidence_experiment_bundles
                     WHERE experiment_id=?1 AND phase=?2",
                    params![
                        signed_for_store.evidence.experiment_id,
                        phase_name(signed_for_store.evidence.phase)
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(EvidenceError::sqlite)?;
            if existing.is_some() {
                return Err(EvidenceError::ExperimentStateConflict);
            }
            transaction
                .execute(
                    "INSERT INTO evidence_experiment_bundles(
                       id, experiment_id, phase, evidence_json, digest_sha256,
                       signer_key_id, signature_hex, recorded_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        signed_for_store.id,
                        signed_for_store.evidence.experiment_id,
                        phase_name(signed_for_store.evidence.phase),
                        canonical_json,
                        signed_for_store.digest_sha256,
                        signed_for_store.signer_key_id,
                        signed_for_store.signature_hex,
                        recorded_at
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            let next_revision = expected_revision
                .checked_add(1)
                .ok_or(EvidenceError::CountTooLarge)?;
            let bundle_column = phase_bundle_column(signed_for_store.evidence.phase);
            let update = format!(
                "UPDATE evidence_self_change_experiments
                 SET status=?2, state_revision=?3, {bundle_column}=?4
                 WHERE id=?1 AND state_revision=?5"
            );
            let changed = transaction
                .execute(
                    &update,
                    params![
                        signed_for_store.evidence.experiment_id,
                        experiment_status_name(next_status),
                        as_i64(next_revision)?,
                        signed_for_store.id,
                        as_i64(expected_revision)?
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
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
                        signed_for_store.evidence.experiment_id,
                        as_i64(next_revision)?,
                        experiment_status_name(expected_status),
                        experiment_status_name(next_status),
                        principal_digest,
                        if passed {
                            "registered evaluations passed"
                        } else {
                            "registered evaluations regressed"
                        },
                        recorded_at
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            let proposal_status = match next_status {
                SelfChangeExperimentStatus::Completed => Some("completed"),
                SelfChangeExperimentStatus::Failed => Some("rolled_back"),
                _ => None,
            };
            if let Some(proposal_status) = proposal_status {
                finish_proposal_in_transaction(
                    &transaction,
                    &proposal_id,
                    proposal_status,
                    &principal_digest,
                    Some(if passed {
                        "experiment completed without registered regressions"
                    } else {
                        "experiment stopped before merge after registered regression"
                    }),
                    recorded_at,
                )?;
            }
            transaction.commit().map_err(EvidenceError::sqlite)
        })
        .await?;
        Ok(signed_bundle)
    }

    pub async fn experiment_evidence(
        &self,
        experiment_id: String,
        phase: ExperimentPhase,
    ) -> Result<Option<SignedExperimentEvidence>, EvidenceError> {
        self.run(move |connection| {
            connection
                .query_row(
                    "SELECT id, evidence_json, digest_sha256, signer_key_id, signature_hex
                     FROM evidence_experiment_bundles
                     WHERE experiment_id=?1 AND phase=?2",
                    params![experiment_id, phase_name(phase)],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(EvidenceError::sqlite)?
                .map(|row| {
                    Ok(SignedExperimentEvidence {
                        id: row.0,
                        evidence: serde_json::from_str(&row.1)
                            .map_err(|_| EvidenceError::InvalidExperimentData)?,
                        digest_sha256: row.2,
                        signer_key_id: row.3,
                        signature_hex: row.4,
                    })
                })
                .transpose()
        })
        .await
    }
}

fn validate_request(request: &RecordExperimentEvidence) -> Result<(), EvidenceError> {
    if !valid_key(&request.id)
        || request.expected_state_revision == 0
        || !valid_sha256(&request.principal_digest)
    {
        return Err(EvidenceError::InvalidExperimentEvidence);
    }
    Ok(())
}

fn validate_phase_commit(
    request: &RecordExperimentEvidence,
    experiment: &crate::evidence::StoredSelfChangeExperiment,
) -> Result<(), EvidenceError> {
    let valid = match request.evidence.phase {
        ExperimentPhase::Baseline => {
            request.evidence.workspace_commit == experiment.definition.base_commit
        }
        ExperimentPhase::Candidate => true,
        ExperimentPhase::Observation => experiment
            .merge_commit
            .as_ref()
            .is_some_and(|commit| commit == &request.evidence.workspace_commit),
    };
    if valid {
        Ok(())
    } else {
        Err(EvidenceError::InvalidExperimentEvidence)
    }
}

fn phase_transition(
    phase: ExperimentPhase,
) -> (SelfChangeExperimentStatus, SelfChangeExperimentStatus) {
    match phase {
        ExperimentPhase::Baseline => (
            SelfChangeExperimentStatus::Prepared,
            SelfChangeExperimentStatus::Prepared,
        ),
        ExperimentPhase::Candidate => (
            SelfChangeExperimentStatus::Prepared,
            SelfChangeExperimentStatus::CandidateEvaluated,
        ),
        ExperimentPhase::Observation => (
            SelfChangeExperimentStatus::Observing,
            SelfChangeExperimentStatus::Completed,
        ),
    }
}

fn failed_status(phase: ExperimentPhase) -> SelfChangeExperimentStatus {
    match phase {
        ExperimentPhase::Observation => SelfChangeExperimentStatus::RollbackRequired,
        ExperimentPhase::Baseline | ExperimentPhase::Candidate => {
            SelfChangeExperimentStatus::Failed
        }
    }
}

fn phase_name(phase: ExperimentPhase) -> &'static str {
    match phase {
        ExperimentPhase::Baseline => "baseline",
        ExperimentPhase::Candidate => "candidate",
        ExperimentPhase::Observation => "observation",
    }
}

fn phase_bundle_column(phase: ExperimentPhase) -> &'static str {
    match phase {
        ExperimentPhase::Baseline => "baseline_bundle_id",
        ExperimentPhase::Candidate => "candidate_bundle_id",
        ExperimentPhase::Observation => "observation_bundle_id",
    }
}
