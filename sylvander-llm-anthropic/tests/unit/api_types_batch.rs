use super::*;
use crate::api::types::{MessageKind, MessageRole, Usage};
use serde_json::json;

#[test]
fn message_batch_round_trip() {
    let batch = MessageBatch {
        id: "msgbatch_abc".to_string(),
        kind: MessageBatchKind::MessageBatch,
        created_at: "2026-07-05T00:00:00Z".to_string(),
        expires_at: "2026-07-06T00:00:00Z".to_string(),
        processing_status: ProcessingStatus::InProgress,
        request_counts: MessageBatchRequestCounts {
            canceled: 0,
            errored: 0,
            expired: 0,
            processing: 5,
            succeeded: 0,
        },
        archived_at: None,
        cancel_initiated_at: None,
        ended_at: None,
        results_url: None,
    };
    let json = serde_json::to_string(&batch).unwrap();
    let back: MessageBatch = serde_json::from_str(&json).unwrap();
    assert_eq!(back, batch);
}

#[test]
fn batch_result_succeeded_round_trip() {
    let result = MessageBatchResult::Succeeded {
        message: Message {
            id: "msg_1".to_string(),
            kind: MessageKind::Message,
            role: MessageRole::Assistant,
            content: vec![],
            model: "claude-sonnet-5-20260601".to_string(),
            stop_reason: None,
            stop_sequence: None,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        },
    };
    let json = serde_json::to_string(&result).unwrap();
    let back: MessageBatchResult = serde_json::from_str(&json).unwrap();
    assert_eq!(back, result);
}

#[test]
fn batch_result_errored_round_trip() {
    let result = MessageBatchResult::Errored {
        error: BatchError {
            kind: BatchErrorKind::Error,
            message: "rate limited".to_string(),
        },
    };
    let json = serde_json::to_string(&result).unwrap();
    assert!(json.contains(r#""type":"errored""#));
    let back: MessageBatchResult = serde_json::from_str(&json).unwrap();
    assert_eq!(back, result);
}

#[test]
fn batch_result_canceled_round_trip() {
    let result = MessageBatchResult::Canceled;
    let json = serde_json::to_string(&result).unwrap();
    assert_eq!(json, r#"{"type":"canceled"}"#);
    let back: MessageBatchResult = serde_json::from_str(&json).unwrap();
    assert_eq!(back, result);
}

#[test]
fn batch_result_expired_round_trip() {
    let result = MessageBatchResult::Expired;
    let json = serde_json::to_string(&result).unwrap();
    assert_eq!(json, r#"{"type":"expired"}"#);
    let back: MessageBatchResult = serde_json::from_str(&json).unwrap();
    assert_eq!(back, result);
}

#[test]
fn request_counts_default_is_all_zeros() {
    let counts = MessageBatchRequestCounts::default();
    assert_eq!(counts.canceled, 0);
    assert_eq!(counts.errored, 0);
    assert_eq!(counts.expired, 0);
    assert_eq!(counts.processing, 0);
    assert_eq!(counts.succeeded, 0);
}

#[test]
fn list_batches_page_round_trip() {
    let json = json!({
        "data": [],
        "has_more": false
    });
    let page: MessageBatchesPage = serde_json::from_value(json).unwrap();
    assert!(page.data.is_empty());
    assert!(!page.has_more);
}
