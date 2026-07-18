use super::*;
use crate::evidence::{EvidenceEvent, FeedbackAttribution, StepStart, TurnStart, feedback_target};
use sylvander_protocol::{FeedbackPrivacyClass, FeedbackRating, RunFeedback};

fn event(id: &str, turn_id: &str, event_type: &str, occurred_at: i64) -> EvidenceEvent {
    EvidenceEvent {
        id: id.into(),
        run_id: "run-analysis".into(),
        session_id: "session-analysis".into(),
        turn_id: Some(turn_id.into()),
        event_type: event_type.into(),
        occurred_at,
        observed_at: occurred_at,
        payload_bytes: 0,
        payload_digest: None,
        payload_json: None,
        privacy_class: "operational".into(),
    }
}

fn feedback(turn_id: &str, privacy_class: FeedbackPrivacyClass) -> RunFeedback {
    RunFeedback {
        target: feedback_target("run-analysis", turn_id),
        rating: FeedbackRating::Positive,
        note: None,
        correction: None,
        tags: Vec::new(),
        task_result: None,
        artifacts: Vec::new(),
        validations: Vec::new(),
        privacy_class,
    }
}

fn attribution() -> FeedbackAttribution {
    FeedbackAttribution {
        principal_digest: "principal".into(),
        channel_instance_id: "tui".into(),
        transport: "unix".into(),
    }
}

#[tokio::test]
async fn cohort_is_stable_and_exposes_missing_or_biased_evidence() {
    let store = EvidenceStore::open_in_memory().await.unwrap();
    store
        .start_run("run-analysis".into(), "test".into(), 1)
        .await
        .unwrap();
    store
        .start_turn(TurnStart {
            id: "turn-success".into(),
            run_id: "run-analysis".into(),
            session_id: "session-analysis".into(),
            agent_id: Some("agent-a".into()),
            started_at: 10,
            input_bytes: 0,
            input_digest: None,
        })
        .await
        .unwrap();
    store
        .start_step(StepStart {
            id: "tool-success".into(),
            turn_id: "turn-success".into(),
            kind: "tool".into(),
            name: "read".into(),
            started_at: 11,
            input_bytes: 0,
            input_digest: None,
        })
        .await
        .unwrap();
    store
        .finish_step("tool-success".into(), 12, "succeeded", 0)
        .await
        .unwrap();
    store
        .record_iteration_usage("turn-success".into(), 10, 5, Some(15))
        .await
        .unwrap();
    for evidence_event in [
        event(
            "approval-request",
            "turn-success",
            "stream_tool_approval_required",
            12,
        ),
        event(
            "approval-decision",
            "turn-success",
            "system_approve_tool",
            13,
        ),
        event("retry", "turn-success", "stream_model_retry", 13),
    ] {
        store.append_event(evidence_event).await.unwrap();
    }
    store
        .record_outcome(
            "outcome-success".into(),
            "turn-success".into(),
            "done".into(),
            true,
            14,
        )
        .await
        .unwrap();
    store
        .finish_turn("turn-success".into(), 14, "succeeded", 0)
        .await
        .unwrap();
    for (privacy, at) in [
        (FeedbackPrivacyClass::Shareable, 15),
        (FeedbackPrivacyClass::MetadataOnly, 16),
    ] {
        store
            .record_feedback(feedback("turn-success", privacy), attribution(), at)
            .await
            .unwrap();
    }

    store
        .start_turn(TurnStart {
            id: "turn-timeout".into(),
            run_id: "run-analysis".into(),
            session_id: "session-analysis".into(),
            agent_id: Some("agent-b".into()),
            started_at: 20,
            input_bytes: 0,
            input_digest: None,
        })
        .await
        .unwrap();
    store
        .record_iteration_usage("turn-timeout".into(), 7, 3, None)
        .await
        .unwrap();
    store
        .append_event(event(
            "timeout",
            "turn-timeout",
            "stream_interaction_timed_out",
            21,
        ))
        .await
        .unwrap();
    store
        .record_outcome(
            "outcome-timeout".into(),
            "turn-timeout".into(),
            "interrupted".into(),
            false,
            22,
        )
        .await
        .unwrap();
    store
        .finish_turn("turn-timeout".into(), 22, "interrupted", 0)
        .await
        .unwrap();
    let query = CohortQuery {
        agent_id: None,
        started_at_inclusive: 0,
        started_before_exclusive: 100,
        privacy_scope: AnalysisPrivacyScope::ShareableOnly,
        limit: 100,
    };
    let first = store.analyze_cohort(query.clone()).await.unwrap();
    let second = store.analyze_cohort(query).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(first.turns.len(), 2);
    assert_eq!(first.success_rate_basis_points, Some(5_000));
    assert_eq!(first.latency_sample_count, 2);
    assert_eq!(first.mean_latency_secs, Some(3));
    assert_eq!(first.p50_latency_secs, Some(2));
    assert_eq!(first.p95_latency_secs, Some(4));
    assert_eq!(first.input_tokens, 17);
    assert_eq!(first.output_tokens, 8);
    assert_eq!(first.fully_priced_cost_nano_usd, None);
    assert_eq!(first.tool_count, 1);
    assert_eq!(first.approval_request_count, 1);
    assert_eq!(first.approval_decision_count, 1);
    assert_eq!(first.retry_count, 1);
    assert_eq!(first.timeout_count, 1);
    assert_eq!(first.positive_feedback_count, 2);
    assert_eq!(
        first.turns[1].failure_class,
        FailureClass::InteractionTimeout
    );
    assert_eq!(first.failure_breakdown.interaction_timeout, 1);
    for warning in [
        AnalysisWarning::MixedAgents,
        AnalysisWarning::IncompletePricing,
        AnalysisWarning::SparseFeedback,
        AnalysisWarning::MixedFeedbackPrivacy,
    ] {
        assert!(first.warnings.contains(&warning), "{warning:?}");
    }

    let limited = store
        .analyze_cohort(CohortQuery {
            agent_id: None,
            started_at_inclusive: 0,
            started_before_exclusive: 100,
            privacy_scope: AnalysisPrivacyScope::ShareableOnly,
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(limited.turns.len(), 1);
    assert!(limited.warnings.contains(&AnalysisWarning::LimitReached));
}
