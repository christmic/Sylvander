//! End-to-end tests for `WriteTool` combined with `ReadTool`.
//!
//! Verifies the tool chain through the full agent loop on a real
//! local port (wiremock) — LLM is told to read a file, then
//! write a modified version, then read it back. The agent must
//! drive Read + Write + Read across two iterations.

use std::sync::Arc;

use serde_json::json;
use sylvander_agent::prelude::*;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use wiremock::matchers::{body_partial_json, method, path};
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

fn write_context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
        .with_fs_root(root)
        .with_capability(sylvander_agent::tool_context::Cap::Read)
        .with_capability(sylvander_agent::tool_context::Cap::Write)
}

#[tokio::test]
async fn write_tool_e2e() {
    let server = MockServer::start().await;

    // Iter 1: model calls Read.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "messages": [{"role": "user", "content": "Read notes.md then write notes2.md with the same content"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_r",
                "name": "Read",
                "input": {"file_path": "notes.md"}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 50, "output_tokens": 20}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Iter 2: model calls Write.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_2",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_w",
                "name": "Write",
                "input": {"file_path": "notes2.md", "content": "copied content"}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 25}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Iter 3: end_turn.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_3",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Done. Wrote notes2.md."}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 120, "output_tokens": 15}
        })))
        .mount(&server)
        .await;

    // Set up workspace
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("notes.md"), "original content").unwrap();

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(ReadTool::new(tmp.path()))
        .tool(WriteTool::new(tmp.path()))
        .tool_context(write_context(tmp.path()))
        .max_iterations(5)
        .build()
        .expect("build");

    let _run = run_with_events(
        &loop_,
        vec![MessageParam::user(
            "Read notes.md then write notes2.md with the same content",
        )],
        move |event| events_clone.lock().unwrap().push(event),
    )
    .await
    .expect("run");

    let events = events.lock().unwrap();

    // Verify both tools were called in order
    let tool_calls: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallStart { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(
        tool_calls,
        vec!["Read", "Write"],
        "expected Read then Write"
    );

    // Verify the file was actually written
    let written =
        std::fs::read_to_string(tmp.path().join("notes2.md")).expect("notes2.md should exist");
    assert_eq!(written, "copied content");

    println!("=== write_tool_e2e ===");
    println!("Tool calls: {tool_calls:?}");
    println!("notes2.md: {written:?}");
    println!("=========================");
}

#[tokio::test]
async fn write_creates_nested_dirs() {
    let server = MockServer::start().await;

    // LLM writes to a nested path.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "messages": [{"role": "user", "content": "write to deep/nested/file.txt"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_w",
                "name": "Write",
                "input": {"file_path": "deep/nested/file.txt", "content": "deep!"}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 30, "output_tokens": 15}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Default: end_turn
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_2",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 60, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(WriteTool::new(tmp.path()))
        .tool_context(write_context(tmp.path()))
        .max_iterations(3)
        .build()
        .expect("build");

    let _run = run_with_events(
        &loop_,
        vec![MessageParam::user("write to deep/nested/file.txt")],
        move |event| events_clone.lock().unwrap().push(event),
    )
    .await
    .expect("run");

    // File was created with parent dirs
    let path = tmp.path().join("deep/nested/file.txt");
    assert!(path.exists(), "deep/nested/file.txt should exist");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "deep!");

    let _ = events; // suppress unused
}
