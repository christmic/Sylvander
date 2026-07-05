//! Integration tests for the sync blocking API.

use serde_json::json;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::{MessageParam, StopReason, Usage};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn minimal_request() -> CreateMessageRequest {
    CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Hello")])
        .build()
        .expect("build should succeed")
}

#[test]
fn blocking_create_via_sync_call() {
    // Note: cannot use #[tokio::test] here — this is a plain #[test]
    // that proves the blocking API works in a non-async context.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt should build");
    let server = rt.block_on(MockServer::start());

    rt.block_on(async {
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_block",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "Hi"}],
                "model": "claude-sonnet-5-20260601",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 5, "output_tokens": 2}
            })))
            .mount(&server)
            .await;
    });

    let async_client = rt.block_on(async {
        AnthropicClient::builder()
            .api_key("test-key")
            .base_url(server.uri())
            .build()
            .expect("build should succeed")
    });

    let blocking_client = async_client
        .blocking()
        .expect("blocking client should build");

    let msg = blocking_client
        .messages()
        .create(&minimal_request())
        .expect("blocking create should succeed");

    assert_eq!(msg.id, "msg_block");
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(msg.usage.input_tokens, 5);
}

#[test]
fn blocking_count_tokens_via_sync_call() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt should build");
    let server = rt.block_on(MockServer::start());

    rt.block_on(async {
        Mock::given(method("POST"))
            .and(path("/v1/messages/count_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "input_tokens": 42
            })))
            .mount(&server)
            .await;
    });

    let async_client = rt.block_on(async {
        AnthropicClient::builder()
            .api_key("test-key")
            .base_url(server.uri())
            .build()
            .expect("build should succeed")
    });

    let blocking_client = async_client.blocking().expect("build should succeed");
    let count = blocking_client
        .messages()
        .count_tokens(&minimal_request())
        .expect("count_tokens should succeed");

    assert_eq!(count.input_tokens, 42);
    let _ = std::any::type_name::<Usage>(); // ensure Usage is in scope
}