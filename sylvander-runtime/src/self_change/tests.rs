use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use sylvander_protocol::EvidenceReference;

use super::*;
use crate::evidence::{
    EvaluationBaseline, EvaluationCase, EvaluationDatasetRevision, EvaluationSplit,
    HmacSha256EvidenceSigner, ImprovementProposal, ImprovementProposalStatus, ImprovementRisk,
    ProposalTransition, RegressionMetric, ScoreDirection, ScoringAdapterKind,
    ScoringAdapterRevision,
};

struct SequenceEvaluator {
    values: Mutex<VecDeque<i64>>,
}

#[async_trait]
impl SelfChangeEvaluationExecutor for SequenceEvaluator {
    async fn evaluate(
        &self,
        _workspace: &Path,
        required: &[RequiredEvaluation],
    ) -> Result<Vec<EvaluationMeasurements>, String> {
        let value = self
            .values
            .lock()
            .map_err(display_error)?
            .pop_front()
            .ok_or_else(|| "evaluation sequence exhausted".to_string())?;
        Ok(required
            .iter()
            .map(|required| EvaluationMeasurements {
                baseline_id: required.baseline_id.clone(),
                measurements: vec![MetricMeasurement {
                    metric: "passed".into(),
                    value,
                    sample_count: 1,
                }],
            })
            .collect())
    }
}

#[tokio::test]
async fn reviewed_local_experiment_rolls_back_observed_regression() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init"]);
    git(&source, &["config", "user.name", "Test"]);
    git(&source, &["config", "user.email", "test@example.com"]);
    fs::write(source.join("README.md"), "base\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-m", "base"]);

    let evidence = EvidenceStore::open(temporary.path().join("evidence.sqlite"))
        .await
        .unwrap();
    register_proposal(&evidence).await;
    let evaluator = Arc::new(SequenceEvaluator {
        values: Mutex::new(VecDeque::from([100, 100, 0])),
    });
    let signer = Arc::new(HmacSha256EvidenceSigner::new("local", vec![7; 32]).unwrap());
    let manager = SelfChangeExperimentManager::new(
        evidence.clone(),
        temporary.path().join("runtime"),
        evaluator,
        signer,
    );
    let actor = "9".repeat(64);
    let started = manager
        .start("experiment-1", "proposal-1", &source, &actor, 10)
        .await
        .unwrap();
    assert_eq!(
        started.experiment.status,
        SelfChangeExperimentStatus::Prepared
    );

    fs::write(
        started.lease.effective_workspace.join("feature.txt"),
        "candidate\n",
    )
    .unwrap();
    let candidate = manager
        .evaluate_candidate("experiment-1", &actor, 11)
        .await
        .unwrap();
    assert_eq!(
        candidate.experiment.status,
        SelfChangeExperimentStatus::CandidateEvaluated
    );
    assert!(
        manager
            .merge_approved("experiment-1", &actor, 12)
            .await
            .unwrap_err()
            .contains("human merge approval")
    );

    manager
        .approve_merge("experiment-1", &actor, "Reviewed candidate evidence.", 13)
        .await
        .unwrap();
    let merged = manager
        .merge_approved("experiment-1", &actor, 14)
        .await
        .unwrap();
    assert_eq!(merged.status, SelfChangeExperimentStatus::Merged);
    assert!(source.join("feature.txt").is_file());

    let rolled_back = manager.observe("experiment-1", &actor, 15).await.unwrap();
    assert_eq!(rolled_back.status, SelfChangeExperimentStatus::RolledBack);
    assert!(rolled_back.rollback_commit.is_some());
    assert!(!source.join("feature.txt").exists());
    assert_eq!(
        evidence
            .improvement_proposal("proposal-1".into())
            .await
            .unwrap()
            .unwrap()
            .status,
        ImprovementProposalStatus::RolledBack
    );
}

async fn register_proposal(store: &EvidenceStore) {
    store
        .register_scoring_adapter(ScoringAdapterRevision {
            id: "validation".into(),
            revision: 1,
            kind: ScoringAdapterKind::BooleanValidation,
            metric: "passed".into(),
            config_digest_sha256: "a".repeat(64),
            created_at: 1,
        })
        .await
        .unwrap();
    store
        .register_evaluation_dataset(EvaluationDatasetRevision {
            id: "coding".into(),
            revision: 1,
            name: "Coding".into(),
            cases: vec![
                EvaluationCase {
                    id: "fixture".into(),
                    split: EvaluationSplit::Fixture,
                    input: reference("fixture"),
                    expected: None,
                    scorer_id: "validation".into(),
                    scorer_revision: 1,
                },
                EvaluationCase {
                    id: "held".into(),
                    split: EvaluationSplit::HeldOut,
                    input: reference("held"),
                    expected: None,
                    scorer_id: "validation".into(),
                    scorer_revision: 1,
                },
            ],
            created_at: 2,
        })
        .await
        .unwrap();
    store
        .register_evaluation_baseline(EvaluationBaseline {
            id: "coding-main".into(),
            dataset_id: "coding".into(),
            dataset_revision: 1,
            metrics: vec![RegressionMetric {
                metric: "passed".into(),
                direction: ScoreDirection::HigherIsBetter,
                baseline_value: 100,
                sample_count: 1,
                max_regression_basis_points: 0,
            }],
            recorded_at: 3,
        })
        .await
        .unwrap();
    store
        .register_improvement_proposal(ImprovementProposal {
            id: "proposal-1".into(),
            cohort_digest_sha256: "b".repeat(64),
            evidence: vec![reference("turn")],
            hypothesis: "Improve the local runtime.".into(),
            expected_benefit: "Higher task completion.".into(),
            risk: ImprovementRisk::Medium,
            affected_components: vec!["runtime".into()],
            rollback_plan: "Revert the reviewed merge.".into(),
            required_evaluations: vec![RequiredEvaluation {
                dataset_id: "coding".into(),
                dataset_revision: 1,
                baseline_id: "coding-main".into(),
            }],
            created_by_principal_digest: "8".repeat(64),
            created_at: 4,
        })
        .await
        .unwrap();
    for (revision, status, occurred_at) in [
        (1, ImprovementProposalStatus::ReadyForReview, 5),
        (2, ImprovementProposalStatus::Approved, 6),
    ] {
        store
            .transition_improvement_proposal(ProposalTransition {
                proposal_id: "proposal-1".into(),
                expected_state_revision: revision,
                status,
                principal_digest: "9".repeat(64),
                reason: Some("reviewed".into()),
                occurred_at,
            })
            .await
            .unwrap();
    }
}

fn reference(name: &str) -> EvidenceReference {
    EvidenceReference {
        locator: name.into(),
        digest_sha256: Some("c".repeat(64)),
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}
