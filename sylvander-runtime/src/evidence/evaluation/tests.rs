use super::*;

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
