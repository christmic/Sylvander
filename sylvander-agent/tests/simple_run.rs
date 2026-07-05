//! End-to-end tests for `AgentLoop::run()` against wiremock.
//!
//! Verifies the reactive event stream + iteration flow + re-feed logic
//! without needing a real API.

use std::sync::Arc;

use serde_json::json;
use sylvander_agent::prelude::*;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mock_client(server: &MockServer) -> AnthropicClient {
    AnthropicClient::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .build()
        .expect("client build")
}

fn test_model() -> ModelInfo {
    ModelInfo::builder()
        .id("claude-sonnet-5-20260601")
        .context_window(200_000)
        .max_output_tokens(8192)
        .capability(ModelCapabilities::TOOL_USE)
        .build()
        .expect("model build")
}

#[tokio::test]
async fn single_iteration_end_turn_returns_final_message() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello!"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let mut loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .on_event(move |event| {
            events_clone.lock().unwrap().push(event);
        })
        .build()
        .expect("build");

    let run = loop_
        .run(vec![MessageParam::user("Hi")])
        .await
        .expect("run should succeed");

    assert_eq!(run.final_message.id, "msg_1");
    assert_eq!(run.iterations, 1);
    assert_eq!(run.total_usage.output_tokens, 5);

    let events = events.lock().unwrap();
    let event_kinds: Vec<&'static str> = events
        .iter()
        .map(|e| match e {
            AgentEvent::IterationStart { .. } => "IterationStart",
            AgentEvent::TextChunk(_) => "TextChunk",
            AgentEvent::IterationEnd { .. } => "IterationEnd",
            AgentEvent::Done(_) => "Done",
            _ => "Other",
        })
        .collect();
    assert_eq!(
        event_kinds,
        vec!["IterationStart", "TextChunk", "IterationEnd", "Done"]
    );
}

#[tokio::test]
async fn tool_use_triggers_tool_execution_and_continues() {
    let server = MockServer::start().await;

    // First LLM call: returns tool_use
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({"messages": [{"role": "user", "content": "Get weather"}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
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
            "usage": {"input_tokens": 10, "output_tokens": 5}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second LLM call (after tool result): returns end_turn
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_2",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "It's sunny, 25°C in Tokyo."}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 20, "output_tokens": 8}
        })))
        .mount(&server)
        .await;

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let weather_tool = MockTool::new(
        "get_weather",
        "Get current weather",
        ToolOutput::ok("sunny, 25C"),
    );

    let mut loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(weather_tool)
        .on_event(move |event| {
            events_clone.lock().unwrap().push(event);
        })
        .build()
        .expect("build");

    let run = loop_
        .run(vec![MessageParam::user("Get weather")])
        .await
        .expect("run should succeed");

    assert_eq!(run.final_message.id, "msg_2");
    assert_eq!(run.iterations, 2);

    // Verify tool was called and recorded
    let tool_called = {
        let events = events.lock().unwrap();
        events.iter().any(|e| matches!(e, AgentEvent::ToolCallStart { name, .. } if name == "get_weather"))
    };
    assert!(tool_called);

    // Verify tool result was emitted
    let tool_resulted = {
        let events = events.lock().unwrap();
        events.iter().any(|e| matches!(e, AgentEvent::ToolCallEnd { name, is_error: false, .. } if name == "get_weather"))
    };
    assert!(tool_resulted);
}

#[tokio::test]
async fn max_iterations_limit_enforced() {
    let server = MockServer::start().await;

    // Always returns tool_use → infinite loop scenario
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_loop",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_x",
                "name": "noop",
                "input": {}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .mount(&server)
        .await;

    let noop_tool = MockTool::new("noop", "no-op", ToolOutput::ok(""));

    let mut loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(noop_tool)
        .max_iterations(3)
        .build()
        .expect("build");

    let result = loop_
        .run(vec![MessageParam::user("Loop forever")])
        .await;

    assert!(matches!(
        result,
        Err(AgentLoopError::MaxIterationsReached(3))
    ));
}

#[tokio::test]
async fn tool_error_continues_loop() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({"messages": [{"role": "user", "content": "Try tool"}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_x",
                "name": "failing_tool",
                "input": {}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 5, "output_tokens": 3}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_2",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Acknowledged the error"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    let failing_tool = MockTool::new("failing_tool", "always fails", ToolOutput::err("intentional failure"));

    let mut loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(failing_tool)
        .build()
        .expect("build");

    let run = loop_
        .run(vec![MessageParam::user("Try tool")])
        .await
        .expect("run should succeed even when tool errors");

    assert_eq!(run.iterations, 2);
}

#[tokio::test]
async fn tool_not_found_records_error_and_continues() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({"messages": [{"role": "user", "content": "Try missing"}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_x",
                "name": "missing_tool",
                "input": {}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 5, "output_tokens": 3}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_2",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "OK"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 3}
        })))
        .mount(&server)
        .await;

    // No tools registered
    let mut loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .build()
        .expect("build");

    let run = loop_
        .run(vec![MessageParam::user("Try missing")])
        .await
        .expect("run should succeed");

    assert_eq!(run.iterations, 2);
}

#[tokio::test]
async fn llm_error_propagates_without_retry() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "invalid_request_error",
            "message": "bad input"
        })))
        .mount(&server)
        .await;

    let mut loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .build()
        .expect("build");

    let result = loop_
        .run(vec![MessageParam::user("Hi")])
        .await;

    // 4xx is non-retryable — propagates with retries: 0
    assert!(matches!(result, Err(AgentLoopError::Llm { retries: 0, .. })));
}

#[tokio::test]
async fn event_order_iteration_start_chunks_end() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_x",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "Let me think...", "signature": "sig_1"},
                {"type": "text", "text": "Here's my answer."}
            ],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 10}
        })))
        .mount(&server)
        .await;

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let mut loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .on_event(move |event| {
            events_clone.lock().unwrap().push(event);
        })
        .build()
        .expect("build");

    loop_
        .run(vec![MessageParam::user("Think")])
        .await
        .expect("run");

    let events = events.lock().unwrap();
    let kinds: Vec<&'static str> = events
        .iter()
        .map(|e| match e {
            AgentEvent::IterationStart { .. } => "IterationStart",
            AgentEvent::ThinkingChunk(_) => "ThinkingChunk",
            AgentEvent::TextChunk(_) => "TextChunk",
            AgentEvent::IterationEnd { .. } => "IterationEnd",
            AgentEvent::Done(_) => "Done",
            _ => "Other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "IterationStart",
            "ThinkingChunk",
            "TextChunk",
            "IterationEnd",
            "Done",
        ]
    );
}