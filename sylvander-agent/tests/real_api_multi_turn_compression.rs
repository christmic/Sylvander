//! Real-API compression test with natural multi-turn tool calling.
//!
//! HAPPY PATH: LLM actually calls Read → tool returns big body →
//! L0 offloads → iter 2 LLM continues. Verifies that compression
//! fires during a genuine tool-calling flow, NOT pre-populated.
//!
//! This is the "正向 case" — the real agent loop with compression.

use std::env;
use std::sync::Arc;

use sylvander_agent::compress::disk::{InMemoryToolResultDisk, ToolResultDisk};
use sylvander_agent::compress::layers::tool_result_budget::ToolResultBudgetLayer;
use sylvander_agent::prelude::*;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};

fn optional_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

#[tokio::test]
#[ignore = "requires real API env vars"]
async fn real_api_natural_multi_turn_with_compression() {
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

    // Workspace with a real file. LLM will Read it.
    let tmp = tempfile::tempdir().expect("tempdir");
    let big_body = "Line: ".to_string() + &"x".repeat(8_000);
    std::fs::write(tmp.path().join("data.txt"), &big_body).expect("write");

    let read_tool = ReadTool::new(tmp.path());

    // L0 with tight budget so the 8k file body triggers it.
    let disk = Arc::new(InMemoryToolResultDisk::new());
    let disk_dyn: Arc<dyn ToolResultDisk> = disk.clone();
    let pipeline = CompressionPipeline::builder()
        .layer(ToolResultBudgetLayer::new(disk_dyn).with_max_inline_chars(500))
        .build();

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let loop_ = AgentLoop::builder()
        .client(client)
        .model(model)
        .tool(read_tool)
        .compression_pipeline(pipeline)
        .max_iterations(4)
        .system_prompt(
            "You are a helpful assistant. When asked to read a file, \
             always use the Read tool. After reading, respond briefly.",
        )
        .build()
        .expect("build");

    let prompt = "Read the file data.txt and tell me how many lines it contains.";

    let run_result = run_with_events(&loop_, vec![MessageParam::user(prompt)], move |event| {
        events_clone.lock().unwrap().push(event);
    })
    .await;

    let events = events.lock().unwrap();

    // Event classification
    let tool_calls: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallStart { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let compressed_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Compressed { layers } => Some(layers.clone()),
            _ => None,
        })
        .collect();
    let all_kinds: Vec<String> = events
        .iter()
        .map(|e| match e {
            AgentEvent::IterationStart { iteration } => format!("Start[{iteration}]"),
            AgentEvent::IterationEnd { .. } => "End".into(),
            AgentEvent::ToolCallStart { name, .. } => format!("ToolCallStart({name})"),
            AgentEvent::ToolCallEnd { name, .. } => format!("ToolCallEnd({name})"),
            AgentEvent::TextChunk(t) => format!("TextChunk({}..)", &t[..t.len().min(40)]),
            AgentEvent::ThinkingChunk(t) => format!("Thinking({}..)", &t[..t.len().min(40)]),
            AgentEvent::Compressed { layers } => {
                let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
                format!("Compressed[{}]", names.join(","))
            }
            AgentEvent::Done(_) => "Done".into(),
            AgentEvent::Error(e) => format!("Error({e})"),
            _ => "Other".into(),
        })
        .collect();

    println!("=== real_api_natural_multi_turn_with_compression ===");
    println!("Run result: {run_result:?}");
    println!("Tool calls: {tool_calls:?}");
    println!("Disk writes: {}", disk.write_count());
    println!("Disk ids: {:?}", disk.ids());
    println!("Compressed events: {compressed_events:?}");
    println!("Event trace:");
    for (i, k) in all_kinds.iter().enumerate() {
        println!("  [{i}] {k}");
    }
    println!("=======================================================");

    // === Assertions ===

    // 1. LLM actually called Read — natural tool use.
    assert!(
        tool_calls.contains(&"Read"),
        "LLM should have called Read; got {tool_calls:?}"
    );

    // 2. Multi-turn actually worked (≥ 2 iterations).
    match &run_result {
        Ok(run) => assert!(
            run.iterations >= 2,
            "expected ≥ 2 iterations (tool_use + end_turn), got {}",
            run.iterations
        ),
        Err(e) => panic!("run failed: {e}"),
    }

    // 3. L0 must have offloaded the big body (Read returned 8k chars
    //    and max_inline_chars=500→ L0 triggered).
    assert!(
        disk.write_count() >= 1,
        "L0 should have offloaded the Read result to disk; got 0 writes"
    );

    // 4. A Compressed event with L0's report must exist.
    let l0_fired = compressed_events.iter().any(|layers| {
        layers
            .iter()
            .any(|l| l.name == "tool_result_budget" && l.condensed_count > 0)
    });
    assert!(
        l0_fired,
        "expected at least one Compressed event with L0 active; got {compressed_events:?}"
    );
}
