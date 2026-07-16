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

struct BarrierTool {
    barrier: Arc<tokio::sync::Barrier>,
}

struct ProgressTool;

struct BurstProgressTool;

#[async_trait::async_trait]
impl Tool for ProgressTool {
    fn name(&self) -> &'static str {
        "progress_probe"
    }
    fn description(&self) -> &'static str {
        "emits output before completion"
    }
    fn input_schema(&self) -> InputSchema {
        InputSchema::empty()
    }
    async fn execute(
        &self,
        _ctx: &ToolContext,
        _input: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::ok("first second"))
    }
    async fn execute_streaming(
        &self,
        _ctx: &ToolContext,
        _input: serde_json::Value,
        progress: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError> {
        progress.emit("first ");
        tokio::task::yield_now().await;
        progress.emit("second");
        Ok(ToolOutput::ok("first second"))
    }
}

#[tokio::test]
async fn tool_output_deltas_arrive_before_final_result() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"msg_progress","type":"message","role":"assistant",
            "content":[{"type":"tool_use","id":"probe","name":"progress_probe","input":{}}],
            "model":"claude-sonnet-5-20260601","stop_reason":"tool_use",
            "usage":{"input_tokens":10,"output_tokens":5}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"msg_done","type":"message","role":"assistant",
            "content":[{"type":"text","text":"done"}],
            "model":"claude-sonnet-5-20260601","stop_reason":"end_turn",
            "usage":{"input_tokens":20,"output_tokens":3}
        })))
        .mount(&server)
        .await;
    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(ProgressTool)
        .build()
        .expect("build");
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = events.clone();
    run_with_events(&loop_, vec![MessageParam::user("progress")], move |event| {
        captured.lock().unwrap().push(event);
    })
    .await
    .expect("run");

    let lifecycle = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallOutputDelta { delta, .. } => Some(format!("delta:{delta}")),
            AgentEvent::ToolCallEnd { output, .. } => Some(format!("end:{output}")),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lifecycle,
        ["delta:first ", "delta:second", "end:first second"]
    );
}

#[async_trait::async_trait]
impl Tool for BurstProgressTool {
    fn name(&self) -> &'static str {
        "burst_progress_probe"
    }
    fn description(&self) -> &'static str {
        "emits more progress than the bounded queue can retain"
    }
    fn input_schema(&self) -> InputSchema {
        InputSchema::empty()
    }
    async fn execute(
        &self,
        _ctx: &ToolContext,
        _input: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::ok("complete"))
    }
    async fn execute_streaming(
        &self,
        _ctx: &ToolContext,
        _input: serde_json::Value,
        progress: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError> {
        for index in 0..1_000 {
            progress.emit(format!("{index}\n"));
        }
        Ok(ToolOutput::ok("complete"))
    }
}

async fn run_burst_progress(include_control_tool: bool) -> Vec<AgentEvent> {
    let server = MockServer::start().await;
    let mut content = Vec::new();
    if include_control_tool {
        content.push(json!({
            "type":"tool_use",
            "id":"plan",
            "name":"present_plan",
            "input":{"steps":["Run bounded progress probe"]}
        }));
    }
    content.push(json!({
        "type":"tool_use",
        "id":"burst",
        "name":"burst_progress_probe",
        "input":{}
    }));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"msg_burst","type":"message","role":"assistant",
            "content":content,
            "model":"claude-sonnet-5-20260601","stop_reason":"tool_use",
            "usage":{"input_tokens":10,"output_tokens":5}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"msg_done","type":"message","role":"assistant",
            "content":[{"type":"text","text":"done"}],
            "model":"claude-sonnet-5-20260601","stop_reason":"end_turn",
            "usage":{"input_tokens":20,"output_tokens":3}
        })))
        .mount(&server)
        .await;
    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(BurstProgressTool)
        .build()
        .expect("build");
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = events.clone();
    run_with_events(&loop_, vec![MessageParam::user("burst")], move |event| {
        captured.lock().unwrap().push(event);
    })
    .await
    .expect("run");
    Arc::try_unwrap(events).unwrap().into_inner().unwrap()
}

fn assert_burst_is_bounded(events: &[AgentEvent]) {
    let deltas = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallOutputDelta { id, delta, .. } if id == "burst" => Some(delta),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(deltas.len() < 1_000);
    assert_eq!(
        deltas
            .iter()
            .filter(|delta| delta.contains("progress buffer was full"))
            .count(),
        1
    );
    let marker_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::ToolCallOutputDelta { id, delta, .. }
                    if id == "burst" && delta.contains("progress buffer was full")
            )
        })
        .unwrap();
    let end_index = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolCallEnd { id, .. } if id == "burst"))
        .unwrap();
    assert!(marker_index < end_index);
}

#[tokio::test]
async fn parallel_tool_progress_is_bounded_and_reports_one_omission() {
    assert_burst_is_bounded(&run_burst_progress(false).await);
}

#[tokio::test]
async fn serial_tool_progress_is_bounded_and_reports_one_omission() {
    assert_burst_is_bounded(&run_burst_progress(true).await);
}

#[async_trait::async_trait]
impl Tool for BarrierTool {
    fn name(&self) -> &'static str {
        "parallel_probe"
    }
    fn description(&self) -> &'static str {
        "waits for another invocation"
    }
    fn input_schema(&self) -> InputSchema {
        InputSchema::empty()
    }
    async fn execute(
        &self,
        _ctx: &ToolContext,
        _input: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.barrier.wait().await;
        Ok(ToolOutput::ok("ready"))
    }
}

#[tokio::test]
async fn ordinary_tool_batch_starts_and_executes_concurrently() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"msg_parallel","type":"message","role":"assistant",
            "content":[
                {"type":"tool_use","id":"one","name":"parallel_probe","input":{}},
                {"type":"tool_use","id":"two","name":"parallel_probe","input":{}}
            ],
            "model":"claude-sonnet-5-20260601","stop_reason":"tool_use",
            "usage":{"input_tokens":10,"output_tokens":5}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"msg_done","type":"message","role":"assistant",
            "content":[{"type":"text","text":"done"}],
            "model":"claude-sonnet-5-20260601","stop_reason":"end_turn",
            "usage":{"input_tokens":20,"output_tokens":3}
        })))
        .mount(&server)
        .await;

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(BarrierTool {
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
        })
        .build()
        .expect("build");
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = events.clone();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        run_with_events(&loop_, vec![MessageParam::user("parallel")], move |event| {
            captured.lock().unwrap().push(event);
        }),
    )
    .await
    .expect("parallel tools must not deadlock")
    .expect("run");

    let events = events.lock().unwrap();
    let lifecycle = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallStart { id, .. } => Some(format!("start:{id}")),
            AgentEvent::ToolCallEnd { id, .. } => Some(format!("end:{id}")),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(lifecycle, ["start:one", "start:two", "end:one", "end:two"]);
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
    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .build()
        .expect("build");

    let run = sylvander_agent::prelude::run_with_events(
        &loop_,
        vec![MessageParam::user("Hi")],
        |event| events_clone.lock().unwrap().push(event),
    )
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
        vec!["IterationStart", "TextChunk", "IterationEnd"]
    );
}

#[tokio::test]
async fn tool_use_triggers_tool_execution_and_continues() {
    let server = MockServer::start().await;

    // First LLM call: returns tool_use
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(
            json!({"messages": [{"role": "user", "content": "Get weather"}]}),
        ))
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
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_creation_input_tokens": 7
            }
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
            "usage": {
                "input_tokens": 20,
                "output_tokens": 8,
                "cache_read_input_tokens": 11
            }
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

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(weather_tool)
        .build()
        .expect("build");

    let run = sylvander_agent::prelude::run_with_events(
        &loop_,
        vec![MessageParam::user("Get weather")],
        |event| events_clone.lock().unwrap().push(event),
    )
    .await
    .expect("run should succeed");

    assert_eq!(run.final_message.id, "msg_2");
    assert_eq!(run.iterations, 2);
    assert_eq!(run.total_usage.input_tokens, 30);
    assert_eq!(run.total_usage.output_tokens, 13);
    assert_eq!(run.total_usage.cache_creation_input_tokens, Some(7));
    assert_eq!(run.total_usage.cache_read_input_tokens, Some(11));
    assert_eq!(run.final_message.usage, run.total_usage);

    let iteration_usage = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentEvent::IterationEnd {
                usage,
                provider_usage,
                ..
            } => Some((usage.clone(), provider_usage.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(iteration_usage.len(), 2);
    assert_eq!(iteration_usage[0].0, iteration_usage[0].1);
    assert_eq!(iteration_usage[1].0, run.total_usage);
    assert_eq!(iteration_usage[1].1.input_tokens, 20);
    assert_eq!(iteration_usage[1].1.output_tokens, 8);
    assert_eq!(iteration_usage[1].1.cache_creation_input_tokens, None);
    assert_eq!(iteration_usage[1].1.cache_read_input_tokens, Some(11));

    // Verify tool was called and recorded
    let tool_called = {
        let events = events.lock().unwrap();
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallStart { name, .. } if name == "get_weather"))
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
async fn max_iterations_returns_the_last_partial_message() {
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

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(noop_tool)
        .max_iterations(3)
        .build()
        .expect("build");

    let result =
        sylvander_agent::prelude::run(&loop_, vec![MessageParam::user("Loop forever")]).await;

    let result = result.expect("last partial message remains usable at the iteration cap");
    assert_eq!(result.iterations, 3);
    assert_eq!(result.final_message.id, "msg_loop");
}

#[tokio::test]
async fn tool_error_continues_loop() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(
            json!({"messages": [{"role": "user", "content": "Try tool"}]}),
        ))
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

    let failing_tool = MockTool::new(
        "failing_tool",
        "always fails",
        ToolOutput::err("intentional failure"),
    );

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(failing_tool)
        .build()
        .expect("build");

    let run = sylvander_agent::prelude::run(&loop_, vec![MessageParam::user("Try tool")])
        .await
        .expect("run should succeed even when tool errors");

    assert_eq!(run.iterations, 2);
}

#[tokio::test]
async fn tool_not_found_records_error_and_continues() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(
            json!({"messages": [{"role": "user", "content": "Try missing"}]}),
        ))
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
    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .build()
        .expect("build");

    let run = sylvander_agent::prelude::run(&loop_, vec![MessageParam::user("Try missing")])
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

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .build()
        .expect("build");

    let result = sylvander_agent::prelude::run(&loop_, vec![MessageParam::user("Hi")]).await;

    // 4xx is non-retryable — propagates with retries: 0
    assert!(matches!(
        result,
        Err(AgentLoopError::Llm { retries: 0, .. })
    ));
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
    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .build()
        .expect("build");

    sylvander_agent::prelude::run_with_events(
        &loop_,
        vec![MessageParam::user("Think")],
        move |event| events_clone.lock().unwrap().push(event),
    )
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
        ]
    );
}
