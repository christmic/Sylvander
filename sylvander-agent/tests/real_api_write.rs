//! Real-API test: `WriteTool` against `MiniMax-M3`.
//!
//! Asks the LLM to read a file then write a modified version.
//! Verifies the tool chain end-to-end with a real LLM.

use std::env;
use std::sync::Arc;

use sylvander_agent::prelude::*;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};

fn optional_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

#[tokio::test]
#[ignore = "requires real API env vars"]
async fn real_api_write_tool_e2e() {
    let Some(token) =
        optional_env("ANTHROPIC_AUTH_TOKEN").or_else(|| optional_env("ANTHROPIC_API_KEY"))
    else {
        eprintln!("token missing; skipping");
        return;
    };
    let Some(base_url) = optional_env("ANTHROPIC_BASE_URL") else {
        eprintln!("ANTHROPIC_BASE_URL missing; skipping");
        return;
    };
    let Some(model_id) = optional_env("SYLVANDER_MODEL") else {
        eprintln!("SYLVANDER_MODEL missing; skipping");
        return;
    };

    let client = AnthropicClient::builder()
        .api_key(&token)
        .base_url(&base_url)
        .build()
        .expect("build client");
    let model = ModelInfo::builder()
        .id(&model_id)
        .context_window(200_000)
        .max_output_tokens(2048)
        .capability(ModelCapabilities::TOOL_USE)
        .build()
        .expect("build model");

    // Set up workspace with a file the LLM can read and modify.
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("greeting.txt"), "Hello, world!").expect("write");

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let loop_ = AgentLoop::builder()
        .client(client)
        .model(model)
        .tool(ReadTool::new(tmp.path()))
        .tool(WriteTool::new(tmp.path()))
        .max_iterations(4)
        .build()
        .expect("build");

    let prompt = "Read the file at greeting.txt. Then write a new \
                  file at farewell.txt containing the text \"Goodbye, world!\".";

    let _run = run_with_events(&loop_, vec![MessageParam::user(prompt)], move |event| {
        events_clone.lock().unwrap().push(event);
    })
    .await
    .expect("run against real API");

    let events = events.lock().unwrap();
    let tool_calls: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallStart { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    let text_chunks: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextChunk(t) => Some(t.clone()),
            _ => None,
        })
        .collect();

    println!("=== real_api_write_tool_e2e ===");
    println!("Tool calls: {tool_calls:?}");
    println!("Text chunks: {text_chunks:?}");
    println!("=========================");

    // LLM should have called Read then Write.
    assert!(
        tool_calls.contains(&"Read"),
        "LLM should have called Read; got {tool_calls:?}"
    );
    assert!(
        tool_calls.contains(&"Write"),
        "LLM should have called Write; got {tool_calls:?}"
    );

    // Verify the file was actually written.
    let farewell = tmp.path().join("farewell.txt");
    assert!(
        farewell.exists(),
        "farewell.txt should exist after Write call"
    );
    let content = std::fs::read_to_string(&farewell).unwrap();
    assert_eq!(content, "Goodbye, world!");
}
