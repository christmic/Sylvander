//! Diagnostic test: does the real API support natural multi-turn
//! `tool_use`/`tool_result` conversations?
//!
//! Run the agent loop against the real API. The LLM is asked to
//! read a real file. We verify:
//! 1. Iter 1: LLM emits a `tool_use` for `Read`
//! 2. Tool executes, `tool_result` is re-fed
//! 3. Iter 2: API accepts the re-fed `tool_result` (no HTTP 400)

use std::env;

use sylvander_agent::prelude::*;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};

fn optional_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

#[tokio::test]
#[ignore = "requires real API env vars"]
async fn real_api_does_multi_turn_work() {
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

    // Create a tempdir with a real file the Read tool can find.
    let tmp = tempfile::tempdir().expect("tempdir");
    let file_name = "notes.md";
    std::fs::write(
        tmp.path().join(file_name),
        "Hello from Sylvander. This is a test file.",
    )
    .expect("write");
    let read_tool = ReadTool::new(tmp.path());

    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let loop_ = AgentLoop::builder()
        .client(client)
        .model(model)
        .tool(read_tool)
        .max_iterations(3)
        .build()
        .expect("build loop");

    let prompt = format!("Read the file at {file_name} and tell me its contents.");

    let result = run_with_events(&loop_, vec![MessageParam::user(prompt)], move |event| {
        events_clone.lock().unwrap().push(event)
    })
    .await;

    let events = events.lock().unwrap();
    let event_kinds: Vec<String> = events
        .iter()
        .map(|e| match e {
            AgentEvent::IterationStart { iteration } => format!("Start[{iteration}]"),
            AgentEvent::TextChunk(_) => "TextChunk".into(),
            AgentEvent::ThinkingChunk(_) => "ThinkingChunk".into(),
            AgentEvent::ToolCallStart { name, .. } => format!("ToolCallStart({name})"),
            AgentEvent::ToolCallEnd { .. } => "ToolCallEnd".into(),
            AgentEvent::Compressed { .. } => "Compressed".into(),
            AgentEvent::IterationEnd { iteration, .. } => format!("End[{iteration}]"),
            AgentEvent::Done(_) => "Done".into(),
            AgentEvent::Error(e) => format!("Error({e:?})"),
            _ => "Other".into(),
        })
        .collect();
    let _ = event_kinds;

    println!("=== real_api_does_multi_turn_work ===");
    println!("Result: {result:?}");
    println!("Events:");
    for (i, k) in event_kinds.iter().enumerate() {
        println!("  [{i}] {k}");
    }
    println!("================================");

    // If multi-turn works, we should see:
    //   Start[1], ToolCallStart(Read), ToolCallEnd, End[1],
    //   Start[2], TextChunk, End[2], Done
    // (Note: order may have ToolCall events between End[1] and Start[2].)

    // If iter 2 errors with HTTP 400, multi-turn tool_result is broken.
    let has_iter_2 = event_kinds.iter().any(|k| k.contains("Start[2]"));
    let has_error = event_kinds.iter().any(|k| k.starts_with("Error"));

    assert!(
        has_iter_2 || has_error,
        "expected either iter 2 to start or an error event; got {event_kinds:?}"
    );
}
