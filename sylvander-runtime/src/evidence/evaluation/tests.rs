use super::*;
use crate::evidence::evaluation_types::{
    EvaluationBaseline, EvaluationCase, EvaluationDatasetRevision, EvaluationSplit,
    RegressionMetric, ScoreDirection,
};
use sylvander_protocol::EvidenceReference;

fn scorer(revision: u64, metric: &str) -> ScoringAdapterRevision {
    ScoringAdapterRevision {
        id: "validation".into(),
        revision,
        kind: ScoringAdapterKind::BooleanValidation,
        metric: metric.into(),
        config_digest_sha256: "a".repeat(64),
        created_at: i64::try_from(revision).unwrap(),
    }
}

#[tokio::test]
async fn scoring_adapter_revisions_are_immutable_sequential_and_durable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("evidence.db");
    let store = EvidenceStore::open(&path).await.unwrap();
    let first = scorer(1, "passed");
    let digest = store.register_scoring_adapter(first.clone()).await.unwrap();
    assert_eq!(
        store.register_scoring_adapter(first.clone()).await.unwrap(),
        digest
    );

    let mut changed = first.clone();
    changed.metric = "changed".into();
    assert!(matches!(
        store.register_scoring_adapter(changed).await.unwrap_err(),
        EvidenceError::EvaluationRevisionConflict
    ));
    assert!(matches!(
        store
            .register_scoring_adapter(scorer(3, "passed"))
            .await
            .unwrap_err(),
        EvidenceError::EvaluationRevisionConflict
    ));
    let second = scorer(2, "passed");
    store
        .register_scoring_adapter(second.clone())
        .await
        .unwrap();
    drop(store);

    let reopened = EvidenceStore::open(path).await.unwrap();
    assert_eq!(
        reopened
            .scoring_adapter("validation".into(), 1)
            .await
            .unwrap(),
        Some(first)
    );
    assert_eq!(
        reopened
            .scoring_adapter("validation".into(), 2)
            .await
            .unwrap(),
        Some(second)
    );
}

fn case(id: &str, split: EvaluationSplit, scorer_id: &str) -> EvaluationCase {
    EvaluationCase {
        id: id.into(),
        split,
        input: EvidenceReference {
            locator: format!("fixture:{id}:input"),
            digest_sha256: Some("b".repeat(64)),
        },
        expected: Some(EvidenceReference {
            locator: format!("fixture:{id}:expected"),
            digest_sha256: Some("c".repeat(64)),
        }),
        scorer_id: scorer_id.into(),
        scorer_revision: 1,
    }
}

#[tokio::test]
async fn dataset_requires_registered_scorers_and_both_deterministic_splits() {
    let store = EvidenceStore::open_in_memory().await.unwrap();
    store
        .register_scoring_adapter(scorer(1, "passed"))
        .await
        .unwrap();
    let definition = EvaluationDatasetRevision {
        id: "coding-core".into(),
        revision: 1,
        name: "Coding core".into(),
        cases: vec![
            case("held-z", EvaluationSplit::HeldOut, "validation"),
            case("fixture-a", EvaluationSplit::Fixture, "validation"),
        ],
        created_at: 10,
    };
    let digest = store
        .register_evaluation_dataset(definition.clone())
        .await
        .unwrap();
    assert_eq!(
        store.register_evaluation_dataset(definition).await.unwrap(),
        digest
    );
    let stored = store
        .evaluation_dataset("coding-core".into(), 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.digest_sha256, digest);
    assert_eq!(stored.definition.cases[0].id, "fixture-a");
    assert_eq!(stored.definition.cases[1].id, "held-z");

    let unknown_scorer = EvaluationDatasetRevision {
        id: "invalid".into(),
        revision: 1,
        name: "Invalid".into(),
        cases: vec![
            case("fixture", EvaluationSplit::Fixture, "missing"),
            case("held", EvaluationSplit::HeldOut, "missing"),
        ],
        created_at: 11,
    };
    assert!(matches!(
        store
            .register_evaluation_dataset(unknown_scorer)
            .await
            .unwrap_err(),
        EvidenceError::InvalidEvaluationDefinition
    ));
}

#[tokio::test]
async fn baseline_thresholds_must_match_dataset_scorer_metrics() {
    let store = EvidenceStore::open_in_memory().await.unwrap();
    store
        .register_scoring_adapter(scorer(1, "passed"))
        .await
        .unwrap();
    store
        .register_evaluation_dataset(EvaluationDatasetRevision {
            id: "coding-core".into(),
            revision: 1,
            name: "Coding core".into(),
            cases: vec![
                case("fixture", EvaluationSplit::Fixture, "validation"),
                case("held", EvaluationSplit::HeldOut, "validation"),
            ],
            created_at: 10,
        })
        .await
        .unwrap();
    let baseline = EvaluationBaseline {
        id: "coding-core-main".into(),
        dataset_id: "coding-core".into(),
        dataset_revision: 1,
        metrics: vec![RegressionMetric {
            metric: "passed".into(),
            direction: ScoreDirection::HigherIsBetter,
            baseline_value: 9_000,
            sample_count: 2,
            max_regression_basis_points: 100,
        }],
        recorded_at: 20,
    };
    let digest = store
        .register_evaluation_baseline(baseline.clone())
        .await
        .unwrap();
    assert_eq!(
        store.register_evaluation_baseline(baseline).await.unwrap(),
        digest
    );
    let stored = store
        .evaluation_baseline("coding-core-main".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.digest_sha256, digest);
    assert_eq!(stored.definition.metrics[0].metric, "passed");

    let invalid = EvaluationBaseline {
        id: "unknown-metric".into(),
        dataset_id: "coding-core".into(),
        dataset_revision: 1,
        metrics: vec![RegressionMetric {
            metric: "invented".into(),
            direction: ScoreDirection::LowerIsBetter,
            baseline_value: 0,
            sample_count: 2,
            max_regression_basis_points: 0,
        }],
        recorded_at: 21,
    };
    assert!(matches!(
        store
            .register_evaluation_baseline(invalid)
            .await
            .unwrap_err(),
        EvidenceError::InvalidEvaluationDefinition
    ));
}
