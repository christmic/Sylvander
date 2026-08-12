use super::*;

#[test]
fn feedback_requires_an_opaque_target_and_has_stable_wire_values() {
    let feedback = RunFeedback {
        target: FeedbackTarget("sha256:opaque".into()),
        rating: FeedbackRating::Negative,
        note: Some("tool changed the wrong file".into()),
        correction: Some("edit src/api.rs instead".into()),
        tags: vec!["correctness".into()],
        task_result: Some(FeedbackTaskResult::Failed),
        artifacts: vec![EvidenceReference {
            locator: "worktree:session-1".into(),
            digest_sha256: None,
        }],
        validations: vec![EvidenceReference {
            locator: "test:cargo-test".into(),
            digest_sha256: Some("a".repeat(64)),
        }],
        privacy_class: FeedbackPrivacyClass::Private,
    };
    let json = serde_json::to_value(&feedback).unwrap();
    assert_eq!(json["rating"], "negative");
    assert_eq!(json["target"], "sha256:opaque");
    assert!(json.get("run_id").is_none());
    assert!(json.get("turn_id").is_none());
    assert_eq!(
        serde_json::from_value::<RunFeedback>(json).unwrap(),
        feedback
    );
}

#[test]
fn feedback_target_accepts_only_the_server_digest_shape() {
    assert!(FeedbackTarget(format!("sha256:{}", "a".repeat(64))).is_well_formed());
    for invalid in [
        format!("sha256:{}", "a".repeat(63)),
        format!("sha256:{}", "A".repeat(64)),
        format!("sha256:{}", "g".repeat(64)),
        "sha256:opaque".into(),
        format!("sha512:{}", "a".repeat(64)),
    ] {
        assert!(
            !FeedbackTarget(invalid.clone()).is_well_formed(),
            "{invalid} must not be accepted as a server-issued target"
        );
    }
}
