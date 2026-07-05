//! Integration tests for `POST /v1/messages` (sync non-streaming create).

use serde_json::json;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::error::AnthropicError;
// ModelId removed; pass model string directly
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::{ContentBlock, InputSchema, MessageParam, StopReason, Tool, ToolChoice};
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
async fn create_simple_text_response() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header("content-type", "application/json"))
        .and(body_partial_json(json!({
            "model": "claude-sonnet-5-20260601",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_abc123",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hi there!"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 4}
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let msg = client
        .messages()
        .create(&minimal_request())
        .await
        .expect("create should succeed");

    assert_eq!(msg.id, "msg_abc123");
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(msg.content.len(), 1);
    match &msg.content[0] {
        ContentBlock::Text(t) => assert_eq!(t.text, "Hi there!"),
        other => panic!("expected Text block, got {other:?}"),
    }
    assert_eq!(msg.usage.input_tokens, 5);
    assert_eq!(msg.usage.output_tokens, 4);
}

#[tokio::test]
async fn create_with_tools_serializes_correctly() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "tools": [{
                "name": "get_weather",
                "description": "Get current weather",
                "input_schema": {
                    "type": "object",
                    "properties": {"location": {"type": "string"}},
                    "required": ["location"]
                }
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_xyz",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_abc",
                "name": "get_weather",
                "input": {"location": "Tokyo"}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 20, "output_tokens": 30}
        })))
        .mount(&server)
        .await;

    let tool = Tool::new(
        "get_weather",
        "Get current weather",
        InputSchema::new_with_properties(
            json!({"location": {"type": "string"}}),
            &["location"],
        ),
    );
    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Weather in Tokyo?")])
        .tool(tool)
        .build()
        .unwrap();

    let client = mock_client(&server);
    let msg = client.messages().create(&req).await.expect("create should succeed");

    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(msg.content.len(), 1);
    let tool_use = msg.first_tool_use().expect("expected tool_use block");
    assert_eq!(tool_use.name, "get_weather");
    assert_eq!(tool_use.input["location"], "Tokyo");
}

#[tokio::test]
async fn create_with_tool_choice_specific_tool() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "tool_choice": {"type": "tool", "name": "Read"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_a",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "OK"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .mount(&server)
        .await;

    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Read foo.txt")])
        .tool(Tool::new("Read", "Read a file", InputSchema::empty()))
        .tool_choice(ToolChoice::tool_serial("Read"))
        .build()
        .unwrap();

    let client = mock_client(&server);
    let msg = client.messages().create(&req).await.expect("create should succeed");
    assert_eq!(msg.id, "msg_a");
}

#[tokio::test]
async fn create_400_api_error_returns_typed_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "invalid_request_error",
            "message": "max_tokens must be > 0",
            "request_id": "req_err"
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let err = client
        .messages()
        .create(&minimal_request())
        .await
        .expect_err("create should fail");
    match err {
        AnthropicError::Api {
            status,
            error_type,
            error_message,
            request_id,
        } => {
            assert_eq!(status, 400);
            assert_eq!(error_type, "invalid_request_error");
            assert_eq!(error_message, "max_tokens must be > 0");
            assert_eq!(request_id.as_deref(), Some("req_err"));
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn create_529_overloaded_is_retryable() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(529).set_body_json(json!({
            "type": "overloaded_error",
            "message": "API is overloaded"
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let err = client
        .messages()
        .create(&minimal_request())
        .await
        .expect_err("create should fail");
    assert!(err.is_retryable());
    assert_eq!(err.status(), Some(529));
}