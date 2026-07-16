use super::*;
use crate::evidence::{
    EvaluationBaseline, EvaluationCase, EvaluationDatasetRevision, EvaluationSplit,
    ProposalTransition, RegressionMetric, ScoreDirection, ScoringAdapterKind,
    ScoringAdapterRevision,
};

async fn evaluation_fixture(store: &EvidenceStore) {
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
    let case = |id: &str, split| EvaluationCase {
        id: id.into(),
        split,
        input: EvidenceReference {
            locator: format!("fixture:{id}:input"),
            digest_sha256: Some("b".repeat(64)),
        },
        expected: None,
        scorer_id: "validation".into(),
        scorer_revision: 1,
    };
    store
        .register_evaluation_dataset(EvaluationDatasetRevision {
            id: "coding-core".into(),
            revision: 1,
            name: "Coding core".into(),
            cases: vec![
                case("fixture", EvaluationSplit::Fixture),
                case("held", EvaluationSplit::HeldOut),
            ],
            created_at: 2,
        })
        .await
        .unwrap();
    store
        .register_evaluation_baseline(EvaluationBaseline {
            id: "coding-main".into(),
            dataset_id: "coding-core".into(),
            dataset_revision: 1,
            metrics: vec![RegressionMetric {
                metric: "passed".into(),
                direction: ScoreDirection::HigherIsBetter,
                baseline_value: 10_000,
                sample_count: 2,
                max_regression_basis_points: 0,
            }],
            recorded_at: 3,
        })
        .await
        .unwrap();
}

fn proposal(baseline_id: &str) -> ImprovementProposal {
    ImprovementProposal {
        id: "proposal-1".into(),
        cohort_digest_sha256: "c".repeat(64),
        evidence: vec![
            EvidenceReference {
                locator: "turn:z".into(),
                digest_sha256: Some("d".repeat(64)),
            },
            EvidenceReference {
                locator: "turn:a".into(),
                digest_sha256: Some("e".repeat(64)),
            },
        ],
        hypothesis: "Reduce tool retry ambiguity.".into(),
        expected_benefit: "Fewer failed coding turns.".into(),
        risk: ImprovementRisk::Medium,
        affected_components: vec!["runtime".into(), "agent".into()],
        rollback_plan: "Discard the experiment worktree.".into(),
        required_evaluations: vec![RequiredEvaluation {
            dataset_id: "coding-core".into(),
            dataset_revision: 1,
            baseline_id: baseline_id.into(),
        }],
        created_by_principal_digest: "f".repeat(64),
        created_at: 4,
    }
}

#[tokio::test]
async fn proposal_is_immutable_canonical_and_requires_real_evaluations() {
    let store = EvidenceStore::open_in_memory().await.unwrap();
    evaluation_fixture(&store).await;
    let definition = proposal("coding-main");
    let digest = store
        .register_improvement_proposal(definition.clone())
        .await
        .unwrap();
    assert_eq!(
        store
            .register_improvement_proposal(definition)
            .await
            .unwrap(),
        digest
    );
    let stored = store
        .improvement_proposal("proposal-1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.digest_sha256, digest);
    assert_eq!(stored.status, ImprovementProposalStatus::Draft);
    assert_eq!(stored.state_revision, 1);
    assert_eq!(stored.definition.evidence[0].locator, "turn:a");
    assert_eq!(
        stored.definition.affected_components,
        vec!["agent", "runtime"]
    );

    let invalid = proposal("missing");
    assert!(matches!(
        store
            .register_improvement_proposal(ImprovementProposal {
                id: "proposal-invalid".into(),
                ..invalid
            })
            .await
            .unwrap_err(),
        EvidenceError::InvalidImprovementProposal
    ));

    let reviewer = "9".repeat(64);
    let ready = store
        .transition_improvement_proposal(ProposalTransition {
            proposal_id: "proposal-1".into(),
            expected_state_revision: 1,
            status: ImprovementProposalStatus::ReadyForReview,
            principal_digest: reviewer.clone(),
            reason: Some("Evidence package is complete.".into()),
            occurred_at: 5,
        })
        .await
        .unwrap();
    assert_eq!(ready.status, ImprovementProposalStatus::ReadyForReview);
    assert_eq!(ready.state_revision, 2);
    assert!(matches!(
        store
            .transition_improvement_proposal(ProposalTransition {
                proposal_id: "proposal-1".into(),
                expected_state_revision: 1,
                status: ImprovementProposalStatus::Approved,
                principal_digest: reviewer.clone(),
                reason: None,
                occurred_at: 6,
            })
            .await
            .unwrap_err(),
        EvidenceError::ProposalStateConflict
    ));
    let approved = store
        .transition_improvement_proposal(ProposalTransition {
            proposal_id: "proposal-1".into(),
            expected_state_revision: 2,
            status: ImprovementProposalStatus::Approved,
            principal_digest: reviewer,
            reason: Some("Approved for isolated evaluation only.".into()),
            occurred_at: 7,
        })
        .await
        .unwrap();
    assert_eq!(approved.status, ImprovementProposalStatus::Approved);
    assert_eq!(approved.state_revision, 3);
}
