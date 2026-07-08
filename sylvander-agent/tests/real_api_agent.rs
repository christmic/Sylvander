//! Integration tests for `AgentLoop` against a real Anthropic-compatible
//! API.
//!
//! All tests are `#[ignore]` so they don't run in regular `cargo test`.
//! Run explicitly with:
//!
//! ```bash
//! ANTHROPIC_AUTH_TOKEN=sk-... \
//! ANTHROPIC_BASE_URL=https://api.minimaxi.com/anthropic \
//! SYLVANDER_MODEL=MiniMax-M3 \
//! SYLVANDER_PROMPT="Reply with just: pong" \
//! cargo test -p sylvander-agent --test real_api_agent -- --ignored
//! ```
//!
//! All configuration is read from env vars — no hardcoded fallbacks.

use std::env;

use futures_util::StreamExt;
use sylvander_agent::prelude::*;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};

/// Read a required env var, returning `None` if missing.
fn optional_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

/// Build a real `AnthropicClient` + `ModelInfo` from env. Returns
/// `None` if required env vars are missing.
fn real_agent() -> Option<(AnthropicClient, ModelInfo)> {
    let auth_token = optional_env("ANTHROPIC_AUTH_TOKEN")
        .or_else(|| optional_env("ANTHROPIC_API_KEY"))?;
    let base_url = optional_env("ANTHROPIC_BASE_URL")?;
    let model_id = optional_env("SYLVANDER_MODEL")?;

    let client = AnthropicClient::builder()
        .api_key(&auth_token)
        .base_url(&base_url)
        .build()
        .ok()?;

    let model = ModelInfo::builder()
        .id(&model_id)
        .context_window(200_000)
        .max_output_tokens(1024)
        .capability(ModelCapabilities::default())
        .build()?;

    Some((client, model))
}

/// Read the prompt from env, or use the default fallback.
fn prompt_from_env() -> String {
    optional_env("SYLVANDER_PROMPT")
        .unwrap_or_else(|| "Reply with just: pong".to_string())
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_AUTH_TOKEN + ANTHROPIC_BASE_URL + SYLVANDER_MODEL env vars"]
async fn real_api_agent_loop_completes() {
    let Some((client, model)) = real_agent() else {
        eprintln!(
            "ANTHROPIC_AUTH_TOKEN / ANTHROPIC_BASE_URL / SYLVANDER_MODEL not set, skipping"
        );
        return;
    };

    let prompt = prompt_from_env();
    eprintln!("=== Real agent loop run ===");
    eprintln!("Model:  {}", model.id);
    eprintln!("Prompt: {prompt}");
    eprintln!();

    let loop_ = AgentLoop::builder()
        .client(client)
        .model(model)
        .max_iterations(3)
        .build()
        .expect("build");

    // Use the new run_stream API (post-R1 refactor)
    let mut events = Box::pin(sylvander_agent::prelude::run_stream(&loop_, vec![MessageParam::user(&prompt)]));
    let mut final_text = String::new();
    let mut final_message: Option<Message> = None;

    while let Some(event) = events.next().await {
        match event {
            AgentEvent::IterationStart { iteration } => {
                eprintln!("[iter {iteration}] start");
            }
            AgentEvent::TextChunk(t) => {
                eprint!("{t}");
                final_text.push_str(&t);
            }
            AgentEvent::ThinkingChunk(t) => {
                eprintln!("[thinking] {t}");
            }
            AgentEvent::ToolCallStart { name, .. } => {
                eprintln!("[tool] {name} called");
            }
            AgentEvent::ToolCallEnd {
                output,
                is_error,
                ..
            } => {
                eprintln!(
                    "[tool] result ({}) {}",
                    if is_error { "error" } else { "ok" },
                    output
                );
            }
            AgentEvent::IterationEnd { usage, .. } => {
                eprintln!(
                    "\n[iter] end ({} in / {} out tokens)",
                    usage.input_tokens, usage.output_tokens
                );
            }
            AgentEvent::Compressed { layers } => {
                let total: u32 = layers.iter().map(|l| l.freed_tokens).sum();
                let removed: usize = layers.iter().map(|l| l.removed_count).sum();
                eprintln!(
                    "[compress] {} layers ran, dropped {} messages, freed ~{} tokens",
                    layers.len(),
                    removed,
                    total
                );
                for layer in layers {
                    eprintln!(
                        "  - {}: removed={} condensed={} freed={}",
                        layer.name,
                        layer.removed_count,
                        layer.condensed_count,
                        layer.freed_tokens
                    );
                }
            }
            AgentEvent::Done(msg) => {
                final_message = Some(msg);
            }
            AgentEvent::Error(e) => {
                eprintln!();
                eprintln!("=== Error ===");
                eprintln!("{e}");
                panic!("agent loop errored: {e}");
            }
            _ => {}
        }
    }

    let msg = final_message.expect("Done event should have fired");
    eprintln!();
    eprintln!("=== Final message ===");
    eprintln!("{}", msg.text());
    eprintln!();
    eprintln!("=== Loop done ===");
    eprintln!("Model: {}", msg.model);
    eprintln!("Stop reason: {:?}", msg.stop_reason);
    eprintln!();

    // Sanity: the final text shouldn't be empty
    assert!(!final_text.is_empty(), "model returned no text");
    assert!(!msg.text().is_empty(), "Message.text() returned empty");
    // Output tokens must be > 0
    assert!(msg.usage.output_tokens > 0, "expected output_tokens > 0");
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_AUTH_TOKEN + ANTHROPIC_BASE_URL + SYLVANDER_MODEL env vars"]
async fn real_api_streaming_events_in_order() {
    let Some((client, model)) = real_agent() else {
        eprintln!("env not set, skipping");
        return;
    };

    let prompt = prompt_from_env();
    let loop_ = AgentLoop::builder()
        .client(client)
        .model(model)
        .max_iterations(3)
        .build()
        .expect("build");

    // Verify event order is exactly: IterationStart, [chunks], IterationEnd, Done
    let mut events = Box::pin(sylvander_agent::prelude::run_stream(&loop_, vec![MessageParam::user(&prompt)]));
    let mut saw_iteration_start = false;
    let mut saw_iteration_end_before_done = false;
    let mut saw_done = false;
    let mut text_chunk_count = 0;

    while let Some(event) = events.next().await {
        match event {
            AgentEvent::IterationStart { .. } => {
                saw_iteration_start = true;
            }
            AgentEvent::TextChunk(_) => text_chunk_count += 1,
            AgentEvent::IterationEnd { .. } => {
                if !saw_done {
                    saw_iteration_end_before_done = true;
                }
            }
            AgentEvent::Done(_) => saw_done = true,
            AgentEvent::Error(e) => panic!("agent loop errored: {e}"),
            _ => {}
        }
    }

    assert!(saw_iteration_start, "expected IterationStart event");
    assert!(
        saw_iteration_end_before_done,
        "expected IterationEnd before Done"
    );
    assert!(saw_done, "expected Done event");
    eprintln!("events: {text_chunk_count} text chunks, end-before-done verified");
}
