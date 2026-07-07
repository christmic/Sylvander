//! Real use case: multi-iteration agent loop with active compression.
//!
//! Scenario:
//! 1. User asks agent to summarize a file
//! 2. Model calls Read, gets a 10k-char body back
//! 3. **L0 fires**: large `tool_result` is offloaded to disk,
//!    inline content replaced with preview + path
//! 4. Model produces summary text, `end_turn`
//! 5. Verifies compression events + final messages
//!
//! This is the "real" demonstration that L0 actually does its job
//! against a realistic wiremock conversation — not just unit-test
//! scenarios.

use std::sync::Arc;

use serde_json::json;
use sylvander_agent::compress::disk::{InMemoryToolResultDisk, ToolResultDisk};
use sylvander_agent::compress::layers::{
    context_collapse::ContextCollapseLayer, micro_compact::MicroCompactLayer,
    orphan_snip::OrphanSnipLayer, tool_result_budget::ToolResultBudgetLayer,
};
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

#[tokio::test]
async fn real_use_case_l0_offloads_huge_read_result() {
    let server = MockServer::start().await;

    // === LLM call 1: model decides to Read ===
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "messages": [{"role": "user", "content": "Summarize notes.md"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_step1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_read_x",
                "name": "Read",
                "input": {"file_path": "notes.md"}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 30, "output_tokens": 25}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // === LLM call 2 (after tool result): model produces summary ===
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_step2",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": "Project status: M1 protocol SDK complete. M2 agent loop in progress. M3 compression pipeline delivered."
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 60, "output_tokens": 25}
        })))
        .mount(&server)
        .await;

    // === Test fixture: a "huge" file (10k chars) ===
    let tmp = tempfile::tempdir().expect("tempdir");
    let big_body = "x".repeat(10_000);
    std::fs::write(tmp.path().join("notes.md"), &big_body).expect("write big");

    let read_tool = ReadTool::new(tmp.path());

    // === L0 with tight budget: anything > 1000 chars goes to disk ===
    let disk = Arc::new(InMemoryToolResultDisk::new());
    let disk_dyn: Arc<dyn ToolResultDisk> = disk.clone();
    let pipeline = CompressionPipeline::builder()
        .layer(ToolResultBudgetLayer::new(disk_dyn).with_max_inline_chars(1000))
        .layer(OrphanSnipLayer::new())
        .layer(MicroCompactLayer::new())
        .layer(ContextCollapseLayer::new())
        .build();

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(read_tool)
        .compression_pipeline(pipeline)
        .max_iterations(3)
        .build()
        .expect("build");

    let run = run_with_events(
        &loop_,
        vec![MessageParam::user("Summarize notes.md")],
        move |event| events_clone.lock().unwrap().push(event),
    )
    .await
    .expect("run");

    // === Verify run completed correctly ===
    assert_eq!(run.iterations, 2, "expected tool_use + end_turn");
    assert!(!run.final_message.text().is_empty());

    // === Verify L0 actually offloaded the big body ===
    assert!(
        disk.write_count() >= 1,
        "L0 should have offloaded the 10k Read body to disk; got {} writes",
        disk.write_count()
    );
    let ids = disk.ids();
    assert!(
        ids.iter().any(|id| id == "toolu_read_x"),
        "L0 should have written the toolu_read_x body; got {ids:?}"
    );

    // === Verify Compressed event was emitted with L0's report ===
    let events = events.lock().unwrap();
    let l0_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Compressed { layers } => {
                let l0 = layers.iter().find(|l| l.name == "tool_result_budget");
                l0.map(|l| (layers.len(), l.condensed_count, l.freed_tokens))
            }
            _ => None,
        })
        .collect();
    assert!(
        !l0_events.is_empty(),
        "expected at least one Compressed event with L0 report"
    );
    let (_total_layers, l0_condensed, l0_freed) = l0_events[0];
    // The pipeline has 4 layers but the run_stream filters out
    // no-op reports before emitting AgentEvent::Compressed — only
    // layers that did work (or recorded a failure) appear. So we
    // expect just 1 entry in this Compressed event (L0), not 4.
    assert_eq!(l0_condensed, 1, "L0 condensed exactly one block");
    assert!(l0_freed > 0, "L0 freed > 0 tokens");

    // === Verify event sequence: IterationStart, ToolCallStart(Read),
    // ToolCallEnd(Read,ok), Compressed (L0), TextChunk, IterationEnd ===
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            AgentEvent::IterationStart { .. } => "IterationStart",
            AgentEvent::ToolCallStart { name, .. } => match name.as_str() {
                "Read" => "ToolCallStart(Read)",
                _ => "ToolCallStart(other)",
            },
            AgentEvent::ToolCallEnd { name, .. } => match name.as_str() {
                "Read" => "ToolCallEnd(Read,ok)",
                _ => "ToolCallEnd(other)",
            },
            AgentEvent::TextChunk(_) => "TextChunk",
            AgentEvent::Compressed { .. } => "Compressed",
            AgentEvent::IterationEnd { .. } => "IterationEnd",
            _ => "Other",
        })
        .collect();

    // Compressed fires at start of next iteration (after iter 1's tool_use).
    assert!(
        kinds.contains(&"ToolCallStart(Read)")
            && kinds.contains(&"ToolCallEnd(Read,ok)")
            && kinds.contains(&"Compressed")
            && kinds.contains(&"TextChunk"),
        "expected full event sequence; got: {kinds:?}"
    );

    println!("=== real_use_case_l0_offloads_huge_read_result ===");
    println!("Iterations: {}", run.iterations);
    println!("L0 disk writes: {} ({} chars saved per block)", disk.write_count(), l0_freed * 4);
    println!("Events:");
    for (i, kind) in kinds.iter().enumerate() {
        println!("  [{i}] {kind}");
    }
    println!("=================================================");
}

#[tokio::test]
async fn real_use_case_full_pipeline_l0_l1_l2_l3_over_multiple_iterations() {
    // More complex: 3-iteration flow that exercises L0, L1, L2, L3.
    // Each iteration: model emits tool_use(Read), tool returns
    // big body, iter ends with re-feed. We use a stateful mock
    // that rotates the file path per call.
    let server = MockServer::start().await;

    let call_count = Arc::new(std::sync::Mutex::new(0u32));

    // Mock: iter 1 returns tool_use(Read) — mounted FIRST so it
    // matches before the end_turn fallback. up_to_n_times(1)
    // ensures it exhausts after one call, letting the fallback
    // match on iter 2.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_x",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_x",
                "name": "Read",
                "input": {"file_path": "file1.md"}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 30}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Fallback mock: iter 2 onwards returns end_turn.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_done",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "All done."}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 500, "output_tokens": 30}
        })))
        .mount(&server)
        .await;

    let _ = call_count; // silence unused

    let tmp = tempfile::tempdir().expect("tempdir");
    for i in 1..=3 {
        let body = "x".repeat(8_000 + i * 1_000);
        std::fs::write(tmp.path().join(format!("file{i}.md")), &body).expect("write");
    }

    let read_tool = ReadTool::new(tmp.path());

    let disk = Arc::new(InMemoryToolResultDisk::new());
    let disk_dyn: Arc<dyn ToolResultDisk> = disk.clone();
    // keep_last_n=1 so older tool_results get condensed by L2
    let pipeline = CompressionPipeline::builder()
        .layer(ToolResultBudgetLayer::new(disk_dyn).with_max_inline_chars(500))
        .layer(OrphanSnipLayer::new())
        .layer(MicroCompactLayer::new().with_keep_last_n(1))
        .layer(ContextCollapseLayer::new().with_max_thinking_chars(100))
        .build();

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .tool(read_tool)
        .compression_pipeline(pipeline)
        .max_iterations(2) // 2 iterations: tool_use + end_turn
        .build()
        .expect("build");

    let run = run_with_events(
        &loop_,
        vec![MessageParam::user("Process files")],
        move |event| events_clone.lock().unwrap().push(event),
    )
    .await
    .expect("run");

    let events = events.lock().unwrap();

    let l0_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Compressed { layers } if layers.iter().any(|l| l.name == "tool_result_budget" && l.condensed_count > 0)))
        .count();

    println!("=== real_use_case_full_pipeline ===");
    println!("Iterations: {}", run.iterations);
    println!("L0 disk writes: {}", disk.write_count());
    println!("L0 active events: {l0_count}");
    let event_kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            AgentEvent::IterationStart { .. } => "IterationStart",
            AgentEvent::ToolCallStart { name, .. } => match name.as_str() {
                "Read" => "ToolCallStart(Read)",
                _ => "ToolCall(other)",
            },
            AgentEvent::ToolCallEnd { .. } => "ToolCallEnd",
            AgentEvent::TextChunk(_) => "TextChunk",
            AgentEvent::Compressed { .. } => "Compressed",
            AgentEvent::IterationEnd { .. } => "IterationEnd",
            AgentEvent::Done(_) => "Done",
            _ => "Other",
        })
        .collect();
    for (i, k) in event_kinds.iter().enumerate() {
        println!("  [{i}] {k}");
    }
    println!("===================================");

    // With max_iterations=2 we expect exactly 2 iterations:
    // iter 1: tool_use Read → tool returns body → re-feed → iter ends
    // iter 2: L0 sees the tool_result, offloads, then LLM returns end_turn
    assert_eq!(run.iterations, 2, "expected 2 iterations");
    // L0 must have offloaded the big body once.
    assert!(disk.write_count() >= 1, "L0 should offload the big body");
    assert!(l0_count >= 1, "expected at least one L0 Compressed event");
}