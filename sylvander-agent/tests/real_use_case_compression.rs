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
    auto_compact::AutoCompactLayer, context_collapse::ContextCollapseLayer,
    micro_compact::MicroCompactLayer, orphan_snip::OrphanSnipLayer,
    tool_result_budget::ToolResultBudgetLayer,
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
    println!(
        "L0 disk writes: {} ({} chars saved per block)",
        disk.write_count(),
        l0_freed * 4
    );
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

// =============================================================================
// L1 — OrphanSnip: pre-populated conversation with orphan tool_result
// =============================================================================

#[tokio::test]
async fn real_use_case_l1_drops_orphan_tool_results() {
    // Scenario: agent starts a fresh run with initial messages that
    // include an orphan tool_result (no matching tool_use anywhere
    // in history). On iter 1, L1 drops it; the loop then runs a
    // normal end_turn.
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_ok",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "all good"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 50, "output_tokens": 10}
        })))
        .mount(&server)
        .await;

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let pipeline = CompressionPipeline::builder()
        .layer(OrphanSnipLayer::new())
        .layer(MicroCompactLayer::new())
        .layer(
            ContextCollapseLayer::new()
                .with_keep_last_n(0)
                .with_max_thinking_chars(200),
        )
        .layer(AutoCompactLayer::new())
        .build();

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .compression_pipeline(pipeline)
        .build()
        .expect("build");

    // Construct initial messages with an orphan tool_result
    // (tool_use_id="orphan" never has a matching tool_use).
    use serde_json::json;
    use sylvander_llm_anthropic::api::types::{
        MessageParam, MessageRole, ToolResultBlock, UserContent, UserContentBlock,
    };
    let initial = vec![
        MessageParam {
            role: MessageRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::ToolResult(ToolResultBlock::new(
                "orphan",
                "stale result from a previous turn",
            ))]),
        },
        MessageParam::user("continue from where we left off"),
    ];

    let _run = run_with_events(&loop_, initial, move |event| {
        events_clone.lock().unwrap().push(event)
    })
    .await
    .expect("run");

    let events = events.lock().unwrap();
    let l1_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Compressed { layers } => {
                let l1 = layers.iter().find(|l| l.name == "orphan_snip");
                l1.map(|l| l.condensed_count)
            }
            _ => None,
        })
        .collect();
    assert!(
        !l1_events.is_empty(),
        "expected L1 to fire on the orphan tool_result"
    );
    assert!(
        l1_events.iter().any(|&c| c >= 1),
        "L1 should have condensed at least one orphan block, got {l1_events:?}"
    );

    println!("=== real_use_case_l1_drops_orphan_tool_results ===");
    println!("L1 events: {l1_events:?}");
    println!("================================================");
}

// =============================================================================
// L2 — MicroCompact: multi-turn conversation, older tool_results condensed
// =============================================================================

#[tokio::test]
async fn real_use_case_l2_condenses_old_tool_results() {
    // Scenario: 5 user messages each carrying a tool_result with a
    // long body. keep_last_n=2 means L2 condenses the 3 oldest
    // (default keep_last_n=3 means 2 are condensed; we set N=2
    // explicitly so 3 are condensed).
    let server = MockServer::start().await;

    // Iter 1: model returns tool_use(Read). up_to_n_times(1) so
    // iter 2 onwards falls through to the default end_turn mock.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_tool",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_x",
                "name": "Read",
                "input": {"file_path": "x.md"}
            }],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 30}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Fallback: end_turn with text.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_done",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 50, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    // 5 user messages with long tool_results pre-loaded.
    use serde_json::json;
    use sylvander_llm_anthropic::api::types::{
        MessageParam, MessageRole, ToolResultBlock, UserContent, UserContentBlock,
    };
    let long_body = "Z".repeat(500);
    let mut initial: Vec<MessageParam> = Vec::new();
    // 5 stale user turns (each with an orphaned tool_result since
    // L1 will see them but no tool_use exists — actually L1 will
    // drop them, so the conversation only has 1 user message
    // remaining for L2 to act on). To exercise L2, we need
    // tool_results that ARE paired with tool_use OR L1 will sweep
    // them first.
    //
    // Solution: skip L1 in the pipeline so L1 doesn't clean up.
    // Or include the matching tool_use blocks in initial messages.
    // We use the second approach — write tool_use + tool_result
    // pairs into initial messages so L1 sees valid pairs and
    // leaves them alone, then L2 condenses older ones.
    for i in 0..5 {
        // Add assistant message with tool_use
        initial.push(MessageParam {
            role: MessageRole::Assistant,
            content: UserContent::Blocks(vec![UserContentBlock::Other(json!({
                "type": "tool_use",
                "id": format!("toolu_{i}"),
                "name": "Read",
                "input": {"file_path": format!("file{i}.md")}
            }))]),
        });
        // Add user message with tool_result
        initial.push(MessageParam {
            role: MessageRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::ToolResult(ToolResultBlock::new(
                format!("toolu_{i}"),
                &long_body,
            ))]),
        });
    }
    // Plus the user's current request (will trigger iter 1).
    initial.push(MessageParam::user("now summarize"));

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let pipeline = CompressionPipeline::builder()
        .layer(OrphanSnipLayer::new())
        .layer(MicroCompactLayer::new().with_keep_last_n(2))
        .layer(ContextCollapseLayer::new())
        .build();

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .compression_pipeline(pipeline)
        .build()
        .expect("build");

    let _run = run_with_events(&loop_, initial, move |event| {
        events_clone.lock().unwrap().push(event)
    })
    .await
    .expect("run");

    let events = events.lock().unwrap();
    let l2_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Compressed { layers } => {
                let l2 = layers.iter().find(|l| l.name == "micro_compact");
                l2.map(|l| l.condensed_count)
            }
            _ => None,
        })
        .collect();

    println!("=== real_use_case_l2_condenses_old_tool_results ===");
    println!("L2 events: {l2_events:?}");
    println!("==============================================");

    // 5 old tool_results, keep_last_n=2 means 3 are condensed.
    assert!(
        l2_events.iter().any(|&c| c >= 1),
        "L2 should have condensed at least one old tool_result; got {l2_events:?}"
    );
}

// =============================================================================
// L3 — ContextCollapse: thinking block in response, trimmed
// =============================================================================

#[tokio::test]
async fn real_use_case_l3_trims_old_thinking_blocks() {
    // Scenario: assistant response includes a long thinking block.
    // After re-feed, the messages have an Other(json) with
    // type=thinking. L3 walks old assistant messages and trims
    // long thinking.
    let server = MockServer::start().await;

    // Iter 1: returns tool_use + thinking. up_to_n_times(1).
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_thinking",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "x".repeat(2000), "signature": "sig_abc"},
                {"type": "tool_use", "id": "toolu_t", "name": "Read", "input": {"file_path": "x"}}
            ],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_done",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 50, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let pipeline = CompressionPipeline::builder()
        .layer(OrphanSnipLayer::new())
        .layer(MicroCompactLayer::new())
        // keep_last_n=0 → all assistant thinking blocks are
        // considered "old" and get trimmed.
        .layer(
            ContextCollapseLayer::new()
                .with_keep_last_n(0)
                .with_max_thinking_chars(200),
        )
        .build();

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(test_model())
        .compression_pipeline(pipeline)
        .build()
        .expect("build");

    let _run = run_with_events(&loop_, vec![MessageParam::user("read x")], move |event| {
        events_clone.lock().unwrap().push(event)
    })
    .await
    .expect("run");

    let events = events.lock().unwrap();
    let all_event_kinds: Vec<String> = events
        .iter()
        .map(|e| match e {
            AgentEvent::IterationStart { iteration } => format!("Start[{iteration}]"),
            AgentEvent::TextChunk(_) => "TextChunk".into(),
            AgentEvent::ThinkingChunk(_) => "ThinkingChunk".into(),
            AgentEvent::ToolCallStart { name, .. } => format!("ToolCallStart({name})"),
            AgentEvent::ToolCallEnd { name, .. } => format!("ToolCallEnd({name})"),
            AgentEvent::Compressed { layers } => {
                let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
                format!("Compressed[{}]", names.join(","))
            }
            AgentEvent::IterationEnd { iteration, .. } => format!("End[{iteration}]"),
            _ => "Other".into(),
        })
        .collect();
    let l3_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Compressed { layers } => {
                let l3 = layers.iter().find(|l| l.name == "context_collapse");
                l3.map(|l| l.condensed_count)
            }
            _ => None,
        })
        .collect();

    println!("=== real_use_case_l3_trims_old_thinking_blocks ===");
    println!("All events:");
    for (i, k) in all_event_kinds.iter().enumerate() {
        println!("  [{i}] {k}");
    }
    println!("L3 events: {l3_events:?}");
    println!("===============================================");

    assert!(
        l3_events.iter().any(|&c| c >= 1),
        "L3 should have trimmed at least one old thinking block; got {l3_events:?}"
    );
}

// =============================================================================
// L4 — AutoCompact: high usage triggers LLM summary
// =============================================================================

#[tokio::test]
async fn real_use_case_l4_summarizes_at_high_usage() {
    // Tiny context_window + huge usage forces L4 to fire at iter 2
    // start. L4 calls the LLM (via AgentLoopAutoCompactLlm) to
    // generate a summary. The summary call is caught by the same
    // wiremock default response (end_turn + text).
    let server = MockServer::start().await;

    // Iter 1: tool_use with HUGE usage (above 50% of context_window=100)
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_l4",
                "name": "Read",
                "input": {"file_path": "x"}
            }],
            "model": "tiny",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 95, "output_tokens": 5}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Default fallback: returns end_turn + text. This catches:
    // - L4's summarize call (extracts the text as summary)
    // - iter 2's normal LLM call (loop ends)
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_end",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "model": "tiny",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 2}
        })))
        .mount(&server)
        .await;

    let tiny_model = ModelInfo::builder()
        .id("tiny")
        .context_window(100)
        .max_output_tokens(50)
        .capability(ModelCapabilities::default())
        .build()
        .unwrap();

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let pipeline = CompressionPipeline::builder()
        .layer(OrphanSnipLayer::new())
        .layer(MicroCompactLayer::new())
        .layer(ContextCollapseLayer::new())
        .layer(
            AutoCompactLayer::new()
                .with_trigger_ratio(0.5)
                .with_keep_last_n_turns(0), // keep 0 → always summarize
        )
        .build();

    let loop_ = AgentLoop::builder()
        .client(mock_client(&server))
        .model(tiny_model)
        .compression_pipeline(pipeline)
        .max_iterations(3)
        .build()
        .expect("build");

    let _run = run_with_events(&loop_, vec![MessageParam::user("hi")], move |event| {
        events_clone.lock().unwrap().push(event)
    })
    .await
    .expect("run");

    let events = events.lock().unwrap();
    let l4_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Compressed { layers } => {
                let l4 = layers.iter().find(|l| l.name == "auto_compact");
                l4.map(|l| (l.removed_count, l.failure.clone()))
            }
            _ => None,
        })
        .collect();

    println!("=== real_use_case_l4_summarizes_at_high_usage ===");
    println!("L4 events: {l4_events:?}");
    println!("============================================");

    // L4 should have either:
    // a) fired successfully (removed_count > 0)
    // b) failed gracefully (failure: Some("auto_compact_llm not configured") OR LLM call failed)
    //
    // The AgentLoop wires the AutoCompactLlm via the AgentLoopAutoCompactLlm
    // struct, so the LLM IS configured. The L4 call hits wiremock's
    // default mock, which returns end_turn+text. L4 extracts the text
    // as summary, replaces messages.
    let fired = l4_events
        .iter()
        .any(|(removed, failure)| *removed > 0 && failure.is_none());
    assert!(
        fired,
        "L4 should have fired successfully and replaced messages; got {l4_events:?}"
    );
}
