//! Local, human-gated self-change experiment orchestration.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use sylvander_protocol::EvidenceReference;

use crate::EvidenceStore;
use crate::evidence::{
    EvaluationComparison, ExperimentEvidenceSigner, ExperimentPhase, ExperimentTransition,
    MetricMeasurement, RecordExperimentEvidence, RequiredEvaluation, SelfChangeExperiment,
    SelfChangeExperimentStatus, SignedExperimentEvidence, StoredSelfChangeExperiment,
    UnsignedExperimentEvidence,
};
use crate::git_worktree::{GitWorktreeManager, PreparedChange, WorkspaceLease};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationMeasurements {
    pub baseline_id: String,
    pub measurements: Vec<MetricMeasurement>,
}

#[async_trait]
pub trait SelfChangeEvaluationExecutor: Send + Sync {
    async fn evaluate(
        &self,
        workspace: &Path,
        required: &[RequiredEvaluation],
    ) -> Result<Vec<EvaluationMeasurements>, String>;
}

#[derive(Debug, Clone)]
pub struct StartedSelfChangeExperiment {
    pub experiment: StoredSelfChangeExperiment,
    pub lease: WorkspaceLease,
    pub baseline_evidence: SignedExperimentEvidence,
}

#[derive(Debug, Clone)]
pub struct CandidateSelfChangeExperiment {
    pub experiment: StoredSelfChangeExperiment,
    pub candidate_evidence: SignedExperimentEvidence,
}

pub struct SelfChangeExperimentManager {
    evidence: EvidenceStore,
    worktrees: GitWorktreeManager,
    evaluator: Arc<dyn SelfChangeEvaluationExecutor>,
    signer: Arc<dyn ExperimentEvidenceSigner>,
}

struct PhaseRecord<'a> {
    experiment: &'a StoredSelfChangeExperiment,
    proposal_digest: &'a str,
    phase: ExperimentPhase,
    workspace_commit: String,
    evaluations: Vec<EvaluationComparison>,
    principal_digest: &'a str,
    occurred_at: i64,
}

impl SelfChangeExperimentManager {
    #[must_use]
    pub fn new(
        evidence: EvidenceStore,
        worktree_base: PathBuf,
        evaluator: Arc<dyn SelfChangeEvaluationExecutor>,
        signer: Arc<dyn ExperimentEvidenceSigner>,
    ) -> Self {
        Self {
            evidence,
            worktrees: GitWorktreeManager::new(worktree_base),
            evaluator,
            signer,
        }
    }

    pub async fn start(
        &self,
        experiment_id: &str,
        proposal_id: &str,
        requested_workspace: &Path,
        principal_digest: &str,
        occurred_at: i64,
    ) -> Result<StartedSelfChangeExperiment, String> {
        let proposal = self
            .evidence
            .improvement_proposal(proposal_id.to_string())
            .await
            .map_err(display_error)?
            .ok_or_else(|| "improvement proposal does not exist".to_string())?;
        let lease = self.worktrees.create(experiment_id, requested_workspace)?;
        let base_commit = self.worktrees.source_commit(&lease)?;
        let experiment = match self
            .evidence
            .register_self_change_experiment(SelfChangeExperiment {
                id: experiment_id.to_string(),
                proposal_id: proposal_id.to_string(),
                lease_id: lease.session_id.clone(),
                branch: lease.branch.clone(),
                base_commit: base_commit.clone(),
                proposal_state_revision: proposal.state_revision,
                started_by_principal_digest: principal_digest.to_string(),
                created_at: occurred_at,
            })
            .await
        {
            Ok(experiment) => experiment,
            Err(error) => {
                let _ = self.worktrees.discard(&lease);
                return Err(error.to_string());
            }
        };
        let comparisons = self
            .evaluate(
                source_workspace(&lease)?,
                &proposal.definition.required_evaluations,
            )
            .await?;
        let baseline_evidence = self
            .record_phase(PhaseRecord {
                experiment: &experiment,
                proposal_digest: &proposal.digest_sha256,
                phase: ExperimentPhase::Baseline,
                workspace_commit: base_commit,
                evaluations: comparisons,
                principal_digest,
                occurred_at,
            })
            .await?;
        let experiment = self.experiment(experiment_id).await?;
        if experiment.status == SelfChangeExperimentStatus::Failed {
            self.worktrees.discard(&lease)?;
            return Err("baseline evaluation regressed".into());
        }
        Ok(StartedSelfChangeExperiment {
            experiment,
            lease,
            baseline_evidence,
        })
    }

    pub async fn evaluate_candidate(
        &self,
        experiment_id: &str,
        principal_digest: &str,
        occurred_at: i64,
    ) -> Result<CandidateSelfChangeExperiment, String> {
        let experiment = self.experiment(experiment_id).await?;
        let proposal = self.proposal(&experiment).await?;
        let lease = self.worktrees.open(&experiment.definition.lease_id)?;
        let prepared = self
            .worktrees
            .prepare_reviewed(&lease)?
            .ok_or_else(|| "candidate worktree has no changes".to_string())?;
        let comparisons = self
            .evaluate(
                lease.effective_workspace.clone(),
                &proposal.definition.required_evaluations,
            )
            .await?;
        let candidate_evidence = self
            .record_phase(PhaseRecord {
                experiment: &experiment,
                proposal_digest: &proposal.digest_sha256,
                phase: ExperimentPhase::Candidate,
                workspace_commit: prepared.candidate_commit,
                evaluations: comparisons,
                principal_digest,
                occurred_at,
            })
            .await?;
        let experiment = self.experiment(experiment_id).await?;
        if experiment.status == SelfChangeExperimentStatus::Failed {
            self.worktrees.discard(&lease)?;
            return Err("candidate evaluation regressed".into());
        }
        Ok(CandidateSelfChangeExperiment {
            experiment,
            candidate_evidence,
        })
    }

    pub async fn approve_merge(
        &self,
        experiment_id: &str,
        principal_digest: &str,
        reason: &str,
        occurred_at: i64,
    ) -> Result<StoredSelfChangeExperiment, String> {
        let experiment = self.experiment(experiment_id).await?;
        self.evidence
            .approve_experiment_merge(ExperimentTransition {
                experiment_id: experiment_id.to_string(),
                expected_state_revision: experiment.state_revision,
                principal_digest: principal_digest.to_string(),
                reason: Some(reason.to_string()),
                occurred_at,
            })
            .await
            .map_err(display_error)
    }

    pub async fn merge_approved(
        &self,
        experiment_id: &str,
        principal_digest: &str,
        occurred_at: i64,
    ) -> Result<StoredSelfChangeExperiment, String> {
        let experiment = self.experiment(experiment_id).await?;
        if experiment.status != SelfChangeExperimentStatus::MergeApproved {
            return Err("experiment does not have human merge approval".into());
        }
        let candidate = self
            .evidence
            .experiment_evidence(experiment_id.to_string(), ExperimentPhase::Candidate)
            .await
            .map_err(display_error)?
            .ok_or_else(|| "candidate evidence is missing".to_string())?;
        let lease = self.worktrees.open(&experiment.definition.lease_id)?;
        let reviewed = self.worktrees.merge_prepared(
            &lease,
            &PreparedChange {
                previous_commit: experiment.definition.base_commit.clone(),
                candidate_commit: candidate.evidence.workspace_commit,
            },
        )?;
        self.evidence
            .record_experiment_merge(
                ExperimentTransition {
                    experiment_id: experiment_id.to_string(),
                    expected_state_revision: experiment.state_revision,
                    principal_digest: principal_digest.to_string(),
                    reason: Some("human-approved candidate merged".into()),
                    occurred_at,
                },
                reviewed.merge_commit,
            )
            .await
            .map_err(display_error)
    }

    pub async fn observe(
        &self,
        experiment_id: &str,
        principal_digest: &str,
        occurred_at: i64,
    ) -> Result<StoredSelfChangeExperiment, String> {
        let merged = self.experiment(experiment_id).await?;
        let observing = self
            .evidence
            .begin_experiment_observation(ExperimentTransition {
                experiment_id: experiment_id.to_string(),
                expected_state_revision: merged.state_revision,
                principal_digest: principal_digest.to_string(),
                reason: Some("begin local post-merge observation".into()),
                occurred_at,
            })
            .await
            .map_err(display_error)?;
        let proposal = self.proposal(&observing).await?;
        let lease = self.worktrees.open(&observing.definition.lease_id)?;
        let merge_commit = observing
            .merge_commit
            .clone()
            .ok_or_else(|| "merge commit is missing".to_string())?;
        let comparisons = self
            .evaluate(
                source_workspace(&lease)?,
                &proposal.definition.required_evaluations,
            )
            .await?;
        self.record_phase(PhaseRecord {
            experiment: &observing,
            proposal_digest: &proposal.digest_sha256,
            phase: ExperimentPhase::Observation,
            workspace_commit: merge_commit.clone(),
            evaluations: comparisons,
            principal_digest,
            occurred_at,
        })
        .await?;
        let observed = self.experiment(experiment_id).await?;
        if observed.status == SelfChangeExperimentStatus::RollbackRequired {
            let rollback_commit = self.worktrees.rollback_reviewed(&lease, &merge_commit)?;
            let rolled_back = self
                .evidence
                .record_experiment_rollback(
                    ExperimentTransition {
                        experiment_id: experiment_id.to_string(),
                        expected_state_revision: observed.state_revision,
                        principal_digest: principal_digest.to_string(),
                        reason: Some("registered observation threshold regressed".into()),
                        occurred_at,
                    },
                    rollback_commit,
                )
                .await
                .map_err(display_error)?;
            self.worktrees.discard(&lease)?;
            return Ok(rolled_back);
        }
        self.worktrees.discard(&lease)?;
        Ok(observed)
    }

    async fn evaluate(
        &self,
        workspace: PathBuf,
        required: &[RequiredEvaluation],
    ) -> Result<Vec<EvaluationComparison>, String> {
        let measurements = self.evaluator.evaluate(&workspace, required).await?;
        let mut comparisons = Vec::with_capacity(measurements.len());
        for result in measurements {
            comparisons.push(
                self.evidence
                    .compare_evaluation_baseline(result.baseline_id, result.measurements)
                    .await
                    .map_err(display_error)?,
            );
        }
        Ok(comparisons)
    }

    async fn record_phase(
        &self,
        record: PhaseRecord<'_>,
    ) -> Result<SignedExperimentEvidence, String> {
        self.evidence
            .record_experiment_evidence(
                RecordExperimentEvidence {
                    id: format!("{}-{:?}", record.experiment.definition.id, record.phase)
                        .to_ascii_lowercase(),
                    expected_state_revision: record.experiment.state_revision,
                    principal_digest: record.principal_digest.to_string(),
                    evidence: UnsignedExperimentEvidence {
                        experiment_id: record.experiment.definition.id.clone(),
                        phase: record.phase,
                        proposal_digest_sha256: record.proposal_digest.to_string(),
                        workspace_commit: record.workspace_commit,
                        evaluations: record.evaluations,
                        artifacts: Vec::<EvidenceReference>::new(),
                        recorded_at: record.occurred_at,
                    },
                },
                self.signer.as_ref(),
            )
            .await
            .map_err(display_error)
    }

    async fn experiment(&self, id: &str) -> Result<StoredSelfChangeExperiment, String> {
        self.evidence
            .self_change_experiment(id.to_string())
            .await
            .map_err(display_error)?
            .ok_or_else(|| "self-change experiment does not exist".to_string())
    }

    async fn proposal(
        &self,
        experiment: &StoredSelfChangeExperiment,
    ) -> Result<crate::evidence::StoredImprovementProposal, String> {
        self.evidence
            .improvement_proposal(experiment.definition.proposal_id.clone())
            .await
            .map_err(display_error)?
            .ok_or_else(|| "improvement proposal does not exist".to_string())
    }
}

fn source_workspace(lease: &WorkspaceLease) -> Result<PathBuf, String> {
    let relative = lease
        .effective_workspace
        .strip_prefix(&lease.worktree_root)
        .map_err(display_error)?;
    Ok(lease.source_root.join(relative))
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
#[path = "../tests/unit/self_change.rs"]
mod tests;
