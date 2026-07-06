//! End-to-end "real use case" test: simulate an agent that reads a
//! file, gets the content, then summarizes it.
//!
//! Even though M2 doesn't ship concrete tools (Read/Bash/Edit come in
//! M3), this test exercises the full `AgentLoop` pipeline end-to-end:
//! - Multi-iteration loop (model → tool → re-feed → `end_turn`)
//! - Reactive event delivery via `run_with_events`
//! - Tool dispatch (mocked `Read` tool returns canned content)
//! - Re-feed logic (`tool_result` blocks)
//! - Final state assembly
//!
//! All against `wiremock`, no real API key needed.

use std::sync::Arc;

use serde_json::json;
use serde_json::Value as JsonValue;
use sylvander_agent::prelude::*;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Simulated file system: tool name → canned file content.
#[derive(Clone)]
struct FakeFileSystem {
    files: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
}

impl FakeFileSystem {
    fn new() -> Self {
        let mut files = std::collections::HashMap::new();
        files.insert(
            "/tmp/notes.md".to_string(),
            "# Project Notes\n- M1 protocol SDK done\n- M2 agent loop in progress\n- M3 tools next".to_string(),
        );
        Self {
            files: Arc::new(std::sync::Mutex::new(files)),
        }
    }
}

/// Mock "Read" tool that returns canned content from the fake FS.
struct ReadTool {
    fs: FakeFileSystem,
}

#[async_trait::async_trait]
impl sylvander_agent::tool::Tool for ReadTool {
    fn name(&self) -> &'static str {
        "Read"
    }
    fn description(&self) -> &'static str {
        "Read a file from disk and return its contents"
    }
    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            json!({"file_path": {"type": "string", "description": "Absolute path to the file"}}),
            &["file_path"],
        )
    }
    async fn execute(
        &self,
        input: JsonValue,
    ) -> Result<ToolOutput, ToolError> {
        let path = input
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Other("missing file_path".into()))?;
        let files = self.fs.files.lock().unwrap();
        match files.get(path) {
            Some(content) => Ok(ToolOutput::ok(content.clone())),
            None => Ok(ToolOutput::err(format!("file not found: {path}"))),
        }
    }
}

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

/// Real use case: agent reads a file then summarizes.
///
/// Conversation flow:
/// 1. User: "Summarize /tmp/notes.md"
/// 2. Model: `tool_use` `Read(/tmp/notes.md)`
/// 3. Loop: execute `Read` → file content
/// 4. Loop: re-feed `tool_result`
/// 5. Model: text "Here's the summary: ..." (`end_turn`)
#[tokio::test]
async fn real_use_case_read_and_summarize() {
    let server = MockServer::start().await;

    // LLM call 1: model decides to read the file
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "messages": [{"role": "user", "content": "Summarize /tmp/notes.md"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_step1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_read_1",
                "name": "Read",
                "input": {"file_path": "/tmp/notes.md"}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 25, "output_tokens": 30}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // LLM call 2: model produces the summary
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_step2",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": "Project status: M1 protocol SDK complete. M2 agent loop in progress (current task). M3 concrete tools (Read/Bash/Edit) are next on the roadmap."
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 80, "output_tokens": 45}
        })))
        .mount(&server)
        .await;

    let fs = FakeFileSystem::new();
    let read_tool = ReadTool { fs: fs.clone() };

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(read_tool)
        .max_iterations(5)
        .build()
        .expect("build");

    // === Run the agent loop with reactive event delivery ===
    let run = sylvander_agent::prelude::run_with_events(
        &loop_,
        vec![MessageParam::user("Summarize /tmp/notes.md")],
        move |event| {
            events_clone.lock().unwrap().push(event);
        },
    )
    .await
    .expect("run should succeed");

    // === Verify the result ===
    assert_eq!(run.iterations, 2, "expected 2 iterations (tool_use + end_turn)");
    assert!(
        run.final_message
            .content
            .iter()
            .any(|b| matches!(b, sylvander_llm_anthropic::api::types::ContentBlock::Text(t) if t.text.contains("M1"))),
        "final message should mention M1"
    );
    assert!(
        run.final_message
            .content
            .iter()
            .any(|b| matches!(b, sylvander_llm_anthropic::api::types::ContentBlock::Text(t) if t.text.contains("M3"))),
        "final message should mention M3"
    );
    assert_eq!(run.total_usage.input_tokens, 80);
    assert_eq!(run.total_usage.output_tokens, 45);

    // === Verify the event stream shows the full flow ===
    let event_log = events.lock().unwrap();
    let mut event_kinds: Vec<&'static str> = Vec::new();
    let mut tool_called = false;
    let mut tool_succeeded = false;

    for event in event_log.iter() {
        match event {
            AgentEvent::IterationStart { .. } => event_kinds.push("IterationStart"),
            AgentEvent::TextChunk(_) => event_kinds.push("TextChunk"),
            AgentEvent::ToolCallStart { name, .. } if name == "Read" => {
                tool_called = true;
                event_kinds.push("ToolCallStart(Read)");
            }
            AgentEvent::ToolCallEnd {
                name,
                is_error: false,
                ..
            } if name == "Read" => {
                tool_succeeded = true;
                event_kinds.push("ToolCallEnd(Read,ok)");
            }
            AgentEvent::IterationEnd { .. } => event_kinds.push("IterationEnd"),
            AgentEvent::Compressed { .. } => event_kinds.push("Compressed"),
            _ => {}
        }
    }
    drop(event_log);

    // Expected event order:
    //   IterationStart, IterationEnd, ToolCallStart(Read),
    //   ToolCallEnd(Read,ok), IterationStart, TextChunk, IterationEnd
    assert_eq!(
        event_kinds,
        vec![
            "IterationStart",
            "IterationEnd",
            "ToolCallStart(Read)",
            "ToolCallEnd(Read,ok)",
            "IterationStart",
            "TextChunk",
            "IterationEnd",
        ],
        "unexpected event sequence"
    );
    assert!(tool_called, "Read tool should have been invoked");
    assert!(tool_succeeded, "Read tool should have succeeded");

    println!("=== real_use_case_read_and_summarize ===");
    println!("Iterations: {}", run.iterations);
    println!("Final message: {}", run.final_message.text());
    println!("Events emitted: {}", event_kinds.len());
    println!("=======================================");
}

/// Real use case: agent handles tool failure gracefully.
///
/// Flow:
/// 1. User: "Read /nonexistent"
/// 2. Model: `tool_use` `Read(/nonexistent)`
/// 3. Loop: execute `Read` → error (file not found)
/// 4. Loop: re-feed `tool_result` with `is_error: true`
/// 5. Model: text "File doesn't exist" (`end_turn`)
#[tokio::test]
async fn real_use_case_tool_error_recovery() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "messages": [{"role": "user", "content": "Read /nonexistent"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_e1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_read_err",
                "name": "Read",
                "input": {"file_path": "/nonexistent"}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 15, "output_tokens": 20}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_e2",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": "The file /nonexistent does not exist on disk."
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 30, "output_tokens": 12}
        })))
        .mount(&server)
        .await;

    let fs = FakeFileSystem::new(); // empty (or only /tmp/notes.md)
    let read_tool = ReadTool { fs: fs.clone() };

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(read_tool)
        .max_iterations(5)
        .build()
        .expect("build");

    let run = sylvander_agent::prelude::run_with_events(
        &loop_,
        vec![MessageParam::user("Read /nonexistent")],
        move |event| {
            events_clone.lock().unwrap().push(event);
        },
    )
    .await
    .expect("run should succeed even with tool error");

    assert_eq!(run.iterations, 2);
    assert!(
        run.final_message
            .content
            .iter()
            .any(|b| matches!(b, sylvander_llm_anthropic::api::types::ContentBlock::Text(t) if t.text.contains("does not exist"))),
        "final message should mention the missing file"
    );

    // Verify the tool error event was fired with is_error: true
    let event_log = events.lock().unwrap();
    let saw_tool_error = event_log
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallEnd { is_error: true, .. }));
    assert!(saw_tool_error, "ToolCallEnd with is_error=true should have fired");

    println!("=== real_use_case_tool_error_recovery ===");
    println!("Tool error correctly flowed back to model");
    println!("Model adapted and produced final response");
    println!("=========================================");
}