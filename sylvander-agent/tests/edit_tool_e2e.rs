//! End-to-end tests for `EditTool` on a real local port (wiremock).
//!
//! Verifies the LLM can call Edit through the agent loop, find a
//! unique string, replace it, and the file is actually modified.

mod support;

use std::sync::Arc;

use serde_json::json;
use sylvander_agent::prelude::*;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use support::qualified_anthropic_loop_builder;

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

fn edit_context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
        .with_fs_root(root)
        .with_capability(sylvander_agent::tool_context::Cap::Read)
        .with_capability(sylvander_agent::tool_context::Cap::Write)
}

#[tokio::test]
async fn edit_tool_e2e() {
    let server = MockServer::start().await;

    // Iter 1: model edits the file.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "change foo to bar in file.txt"}]
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_e",
                "name": "Edit",
                "input": {
                    "file_path": "file.txt",
                    "old_string": "foo",
                    "new_string": "bar"
                }
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 50, "output_tokens": 30}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Iter 2: end_turn.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_2",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Done."}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 60, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    // Workspace: file with "foo" to be replaced.
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("file.txt"), "the foo is here").unwrap();

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let loop_ = qualified_anthropic_loop_builder(mock_client(&server), test_model())
        .tool(EditTool::new())
        .tool_context(edit_context(tmp.path()))
        .max_iterations(3)
        .build()
        .expect("build");

    let _run = run_with_events(
        &loop_,
        vec![MessageParam::user("change foo to bar in file.txt")],
        move |event| events_clone.lock().unwrap().push(event),
    )
    .await
    .expect("run");

    let events = events.lock().unwrap();
    let tool_calls: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallStart { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(tool_calls, vec!["Edit"]);

    // File content was actually changed
    let content = std::fs::read_to_string(tmp.path().join("file.txt")).unwrap();
    assert_eq!(content, "the bar is here");

    println!("=== edit_tool_e2e ===");
    println!("Tool calls: {tool_calls:?}");
    println!("file.txt: {content:?}");
    println!("====================");
}

#[tokio::test]
async fn edit_tool_with_ambiguous_match_returns_error() {
    // LLM calls Edit with old_string appearing twice. Tool returns
    // is_error without modifying the file. LLM should react and
    // re-read or retry — but in this test we just verify the
    // tool behavior.
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "replace x in file.txt"}]
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_e",
                "name": "Edit",
                "input": {
                    "file_path": "file.txt",
                    "old_string": "x",
                    "new_string": "y"
                }
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 30, "output_tokens": 15}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Default fallback
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_2",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 50, "output_tokens": 3}
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir");
    // "x" appears twice — Edit should refuse
    std::fs::write(tmp.path().join("file.txt"), "x and x").unwrap();

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let loop_ = qualified_anthropic_loop_builder(mock_client(&server), test_model())
        .tool(EditTool::new())
        .tool_context(edit_context(tmp.path()))
        .max_iterations(3)
        .build()
        .expect("build");

    let _run = run_with_events(
        &loop_,
        vec![MessageParam::user("replace x in file.txt")],
        move |event| events_clone.lock().unwrap().push(event),
    )
    .await
    .expect("run");

    // File content unchanged
    let content = std::fs::read_to_string(tmp.path().join("file.txt")).unwrap();
    assert_eq!(content, "x and x");

    // Verify the Edit tool was called but reported is_error
    let events = events.lock().unwrap();
    let edit_results: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallEnd { is_error: true, .. } => Some("err"),
            AgentEvent::ToolCallEnd {
                is_error: false, ..
            } => Some("ok"),
            _ => None,
        })
        .collect();
    assert!(
        edit_results.contains(&"err"),
        "Edit should have returned is_error"
    );
}
