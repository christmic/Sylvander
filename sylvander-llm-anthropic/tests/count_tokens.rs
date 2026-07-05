//! Integration tests for `POST /v1/messages/count_tokens`.
//!
//! Uses `wiremock` to simulate the Anthropic API.

use serde_json::json;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::error::AnthropicError;
// ModelId removed; pass model string directly
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::MessageParam;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mock_client(server: &MockServer) -> AnthropicClient {
    AnthropicClient::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .build()
        .expect("build should succeed")
}

fn minimal_request() -> CreateMessageRequest {
    CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Hello")])
        .build()
        .expect("build should succeed")
}

#[tokio::test]
async fn count_tokens_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages/count_tokens"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header("content-type", "application/json"))
        .and(body_partial_json(json!({
            "model": "claude-sonnet-5-20260601",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "input_tokens": 42
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let count = client
        .messages()
        .count_tokens(&minimal_request())
        .await
        .expect("count_tokens should succeed");

    assert_eq!(count.input_tokens, 42);
}

#[tokio::test]
async fn count_tokens_validation_error_empty_messages() {
    let server = MockServer::start().await;
    let client = mock_client(&server);

    // Build a request that bypasses the builder's required-field check by
    // constructing the struct directly with empty messages.
    let req = CreateMessageRequest {
        model: "claude-sonnet-5-20260601".into(),
        max_tokens: 1024,
        messages: vec![],
        system: None,
        tools: vec![],
        tool_choice: None,
        thinking: None,
        output_config: None,
        temperature: None,
        top_p: None,
        top_k: None,
        stop_sequences: vec![],
    };

    let result = client.messages().count_tokens(&req).await;
    assert!(matches!(result, Err(AnthropicError::Validation(_))));
}

#[tokio::test]
async fn count_tokens_400_api_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages/count_tokens"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "invalid_request_error",
            "message": "model is required",
            "request_id": "req_xyz"
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let err = client
        .messages()
        .count_tokens(&minimal_request())
        .await
        .expect_err("count_tokens should fail");
    match &err {
        AnthropicError::Api {
            status,
            error_type,
            error_message,
            request_id,
        } => {
            assert_eq!(*status, 400);
            assert_eq!(error_type, "invalid_request_error");
            assert_eq!(error_message, "model is required");
            assert_eq!(request_id.as_deref(), Some("req_xyz"));
        }
        other => panic!("expected Api error, got {other:?}"),
    }
    assert!(!err.is_retryable());
    assert_eq!(err.status(), Some(400));
}

#[tokio::test]
async fn count_tokens_429_is_retryable() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages/count_tokens"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "type": "rate_limit_error",
            "message": "slow down"
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let err = client
        .messages()
        .count_tokens(&minimal_request())
        .await
        .expect_err("count_tokens should fail");
    assert!(err.is_retryable());
    assert_eq!(err.status(), Some(429));
}

#[tokio::test]
async fn count_tokens_500_is_retryable() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages/count_tokens"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "type": "api_error",
            "message": "internal error"
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let err = client
        .messages()
        .count_tokens(&minimal_request())
        .await
        .expect_err("count_tokens should fail");
    assert!(err.is_retryable());
    assert_eq!(err.status(), Some(500));
}