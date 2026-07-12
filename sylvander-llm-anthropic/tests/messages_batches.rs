//! Integration tests for the Message Batches API.
//!
//! Covers all 4 endpoints: `create`, `retrieve`, `list`, `cancel`.

use serde_json::json;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::error::AnthropicError;
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::{
    BatchRequest, CreateMessageBatchRequest, ListBatchesParams, MessageBatchRequestCounts,
    MessageParam, ProcessingStatus,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mock_client(server: &MockServer) -> AnthropicClient {
    AnthropicClient::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .build()
        .expect("build should succeed")
}

fn sample_batch(id: &str, status: ProcessingStatus) -> serde_json::Value {
    json!({
        "id": id,
        "type": "message_batch",
        "created_at": "2026-07-05T00:00:00Z",
        "expires_at": "2026-07-06T00:00:00Z",
        "processing_status": match status {
            ProcessingStatus::InProgress => "in_progress",
            ProcessingStatus::Canceling => "canceling",
            ProcessingStatus::Ended => "ended",
        },
        "request_counts": {
            "canceled": 0,
            "errored": 0,
            "expired": 0,
            "processing": 5,
            "succeeded": 0,
        }
    })
}

fn sample_create_request() -> CreateMessageBatchRequest {
    let inner = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Hi")])
        .build()
        .unwrap();
    CreateMessageBatchRequest {
        requests: vec![BatchRequest {
            custom_id: "req-001".to_string(),
            params: inner,
        }],
    }
}

#[tokio::test]
async fn batches_create_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages/batches"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(sample_batch("msgbatch_001", ProcessingStatus::InProgress)),
        )
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let batch = client
        .messages()
        .batches()
        .create(&sample_create_request())
        .await
        .expect("create should succeed");
    assert_eq!(batch.id, "msgbatch_001");
    assert_eq!(batch.processing_status, ProcessingStatus::InProgress);
}

#[tokio::test]
async fn batches_retrieve_success() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/messages/batches/msgbatch_001"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(sample_batch("msgbatch_001", ProcessingStatus::InProgress)),
        )
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let batch = client
        .messages()
        .batches()
        .retrieve("msgbatch_001")
        .await
        .expect("retrieve should succeed");
    assert_eq!(batch.id, "msgbatch_001");
}

#[tokio::test]
async fn batches_retrieve_ended_includes_results_url() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/messages/batches/msgbatch_done"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msgbatch_done",
            "type": "message_batch",
            "created_at": "2026-07-04T00:00:00Z",
            "expires_at": "2026-07-05T00:00:00Z",
            "ended_at": "2026-07-04T01:00:00Z",
            "processing_status": "ended",
            "request_counts": {
                "canceled": 0, "errored": 0, "expired": 0, "processing": 0, "succeeded": 5
            },
            "results_url": "https://api.anthropic.com/v1/messages/batches/msgbatch_done/results"
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let batch = client
        .messages()
        .batches()
        .retrieve("msgbatch_done")
        .await
        .expect("retrieve should succeed");
    assert_eq!(batch.processing_status, ProcessingStatus::Ended);
    assert!(batch.results_url.is_some());
    assert!(batch.ended_at.is_some());
}

#[tokio::test]
async fn batches_list_with_pagination() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/messages/batches"))
        .and(query_param("limit", "10"))
        .and(query_param("before_id", "msgbatch_050"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                sample_batch("msgbatch_049", ProcessingStatus::Ended),
                sample_batch("msgbatch_048", ProcessingStatus::Ended),
            ],
            "has_more": true,
            "first_id": "msgbatch_048",
            "last_id": "msgbatch_049"
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let page = client
        .messages()
        .batches()
        .list(Some(&ListBatchesParams {
            limit: Some(10),
            before_id: Some("msgbatch_050".to_string()),
            after_id: None,
        }))
        .await
        .expect("list should succeed");
    assert_eq!(page.data.len(), 2);
    assert!(page.has_more);
    assert_eq!(page.first_id.as_deref(), Some("msgbatch_048"));
}

#[tokio::test]
async fn batches_cancel_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages/batches/msgbatch_001/cancel"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(sample_batch("msgbatch_001", ProcessingStatus::Canceling)),
        )
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let batch = client
        .messages()
        .batches()
        .cancel("msgbatch_001")
        .await
        .expect("cancel should succeed");
    assert_eq!(batch.processing_status, ProcessingStatus::Canceling);
}

#[tokio::test]
async fn batches_create_400_api_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages/batches"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "invalid_request_error",
            "message": "requests must not be empty",
            "request_id": "req_batch_err"
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let result = client
        .messages()
        .batches()
        .create(&sample_create_request())
        .await;
    match result {
        Err(AnthropicError::Api {
            status,
            error_type,
            request_id,
            ..
        }) => {
            assert_eq!(status, 400);
            assert_eq!(error_type, "invalid_request_error");
            assert_eq!(request_id.as_deref(), Some("req_batch_err"));
        }
        Ok(_) => panic!("expected Api error"),
        Err(other) => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn batches_retrieve_404_returns_typed_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/messages/batches/msgbatch_missing"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "type": "not_found_error",
            "message": "batch not found"
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let err = client
        .messages()
        .batches()
        .retrieve("msgbatch_missing")
        .await
        .expect_err("retrieve should fail");
    assert_eq!(err.status(), Some(404));
    assert!(!err.is_retryable());
}

#[test]
fn request_counts_default_construction() {
    let counts = MessageBatchRequestCounts::default();
    assert_eq!(counts.succeeded, 0);
    assert_eq!(counts.processing, 0);
}
