//! End-to-end integration tests for the `ReadTool`.
//!
//! Two layers:
//! - `read_e2e_wiremock` — agent calls Read against a real `tempfile`
//!   workspace; LLM response scripted via `wiremock`
//! - `read_e2e_real_api` — single-iteration call against a real
//!   Anthropic-compatible API (gated by `#[ignore]` + env vars).
//!   Note: the local `MiniMax-M3` proxy at `api.minimaxi.com/anthropic`
//!   returns `invalid params` for multi-turn `tool_use`/`tool_result`
//!   conversations, so the real API test verifies only the
//!   single-iteration `tool_use` response shape. The wiremock tests
//!   cover the full multi-turn flow.

use std::fs;
use std::sync::Arc;

use serde_json::json;
use sylvander_agent::prelude::*;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use tempfile::TempDir;
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

#[tokio::test]
async fn read_e2e_wiremock() {
    // Set up a real file in a temp directory
    let dir = TempDir::new().expect("tempdir");
    let file_path = dir.path().join("notes.md");
    fs::write(
        &file_path,
        "# Project Plan\n- Phase 1: M1 done\n- Phase 2: M2 in progress\n- Phase 3: M3 next",
    )
    .expect("write notes.md");

    let server = MockServer::start().await;

    // LLM call 1: model decides to read the file
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "messages": [{"role": "user", "content": "Read notes.md and summarize"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_r1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_read_1",
                "name": "Read",
                "input": {"file_path": "notes.md"}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 20, "output_tokens": 25}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // LLM call 2: model produces summary from the file content
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_r2",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": "The plan has 3 phases: Phase 1 M1 done, Phase 2 M2 in progress, Phase 3 M3 next."
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 60, "output_tokens": 28}
        })))
        .mount(&server)
        .await;

    let read_tool = ReadTool::new(dir.path());

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
        vec![MessageParam::user("Read notes.md and summarize")],
        move |event| {
            events_clone.lock().unwrap().push(event);
        },
    )
    .await
    .expect("run should succeed");

    // Verify the agent completed in 2 iterations
    assert_eq!(run.iterations, 2);
    // Verify the final response mentions the file content
    let text = run.final_message.text();
    assert!(
        text.contains("Phase 1") && text.contains("Phase 2") && text.contains("Phase 3"),
        "final response should mention all 3 phases, got: {text}"
    );

    // Verify the tool was actually called
    let event_log = events.lock().unwrap();
    let saw_read_call = event_log.iter().any(
        |e| matches!(e, AgentEvent::ToolCallStart { name, .. } if name == "Read"),
    );
    let saw_read_ok = event_log.iter().any(
        |e| matches!(e, AgentEvent::ToolCallEnd { name, is_error: false, .. } if name == "Read"),
    );
    assert!(saw_read_call, "Read tool should have been called");
    assert!(saw_read_ok, "Read tool should have returned ok");
}

#[tokio::test]
async fn read_e2e_wiremock_missing_file() {
    // File does NOT exist — tool returns Ok(ToolOutput::err), loop continues
    let dir = TempDir::new().expect("tempdir");
    // No file written

    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "messages": [{"role": "user", "content": "Read missing.txt"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_m1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_read_1",
                "name": "Read",
                "input": {"file_path": "missing.txt"}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 15}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_m2",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "The file missing.txt does not exist."}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 30, "output_tokens": 8}
        })))
        .mount(&server)
        .await;

    let read_tool = ReadTool::new(dir.path());

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(read_tool)
        .max_iterations(5)
        .build()
        .expect("build");

    let run = sylvander_agent::prelude::run(&loop_, vec![MessageParam::user("Read missing.txt")])
        .await
        .expect("run should succeed even with file-not-found error");

    assert_eq!(run.iterations, 2);
    let text = run.final_message.text();
    assert!(
        text.contains("does not exist"),
        "final response should mention missing file, got: {text}"
    );
}

// =============================================================================
// Real API test (#[ignore] + env var guard)
// =============================================================================
//
// KNOWN LIMITATION: the local MiniMax-M3 proxy at
// `https://api.minimaxi.com/anthropic` returns `invalid params` for the
// SECOND request in a multi-turn tool_use/tool_result conversation. Single-
// iteration tool_use calls work fine. This means the full agent loop
// can't run end-to-end against this specific proxy, but the wiremock
// tests above cover the full flow.
//
// The real API test below verifies the SINGLE-ITERATION case: model
// accepts the Read tool and decides to use it. The full multi-turn
// execution is gated by a working proxy.

fn require_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn real_client() -> Option<(AnthropicClient, ModelInfo)> {
    let token = require_env("ANTHROPIC_AUTH_TOKEN")
        .or_else(|| require_env("ANTHROPIC_API_KEY"))?;
    let base_url = require_env("ANTHROPIC_BASE_URL")?;
    let model_id = require_env("SYLVANDER_MODEL")?;

    let client = AnthropicClient::builder()
        .api_key(&token)
        .base_url(&base_url)
        .build()
        .ok()?;
    let model = ModelInfo::builder()
        .id(&model_id)
        .context_window(200_000)
        .max_output_tokens(2048)
        .capability(ModelCapabilities::TOOL_USE)
        .build()?;
    Some((client, model))
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_AUTH_TOKEN + ANTHROPIC_BASE_URL + SYLVANDER_MODEL"]
async fn read_e2e_real_api_single_iteration() {
    let Some((client, model)) = real_client() else {
        eprintln!("env vars not set, skipping");
        return;
    };

    // Set up a real test file the agent can read
    let test_file = std::path::PathBuf::from("/tmp/sylvander_test_notes.md");
    let test_content = "# Sylvander E2E\nPhase 1: M1 done.\nPhase 2: M2 done.\nPhase 3: M3 in progress.\n";
    if let Err(e) = fs::write(&test_file, test_content) {
        eprintln!("cannot write {test_file:?}: {e}, skipping");
        return;
    }

    let read_tool = ReadTool::new("/tmp");
    let loop_ = AgentLoop::builder()
        .client(client)
        .model(model)
        .tool(read_tool)
        .max_iterations(3) // proxy breaks on iter 2 — use 3 to be safe
        .build()
        .expect("build");

    let prompt = "Read /tmp/sylvander_test_notes.md and list the 3 phase names.";
    eprintln!("=== Real Read e2e ===");
    eprintln!("Prompt: {prompt}");
    eprintln!();

    // Use run_with_events so we can inspect the full event stream.
    // The proxy breaks on multi-turn tool_use/tool_result, so we
    // expect either a clean run (model produces end_turn in iter 1)
    // or a Llm error from iter 2. Either is acceptable — what we
    // MUST verify is that the model received the Read tool and chose
    // to invoke it.
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let result = sylvander_agent::prelude::run_with_events(
        &loop_,
        vec![MessageParam::user(prompt)],
        move |event| {
            events_clone.lock().unwrap().push(event);
        },
    )
    .await;

    eprintln!("=== Run result: {result:?}");
    eprintln!();

    let events = events.lock().unwrap();
    eprintln!("=== Event stream ===");
    for (i, event) in events.iter().enumerate() {
        eprintln!("  [{i}] {event:?}");
    }

    // The CRITICAL assertion: the model received the Read tool and
    // chose to invoke it in iteration 1. This proves the SDK correctly
    // transmits the tool definition to the real API.
    let tool_call_started = events.iter().any(|e| {
        matches!(e, AgentEvent::ToolCallStart { name, .. } if name == "Read")
    });
    assert!(
        tool_call_started,
        "model should have invoked Read tool; events did not include ToolCallStart(Read)"
    );

    // Cleanup
    let _ = fs::remove_file(&test_file);

    // Suppress unused warning — the loop's Err result is acceptable
    // (proxy breaks on iter 2) and not asserted.
    let _ = result;
}
