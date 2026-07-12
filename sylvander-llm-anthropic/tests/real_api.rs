//! Integration tests against the real Anthropic API.
//!
//! All tests are marked `#[ignore]` so they don't run in regular
//! `cargo test`. Run explicitly with:
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-... cargo test --test real_api -- --ignored
//! ```
//!
//! Each test checks for the env var at runtime and `eprintln!`s a
//! skip message if it's missing. This makes them safe to keep in the
//! tree even without a key configured.

use futures_util::StreamExt;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::{MessageParam, RawStreamEvent, StopReason};

fn api_key() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

fn real_client() -> Option<AnthropicClient> {
    let key = api_key()?;
    Some(
        AnthropicClient::builder()
            .api_key(key)
            .build()
            .expect("client should build"),
    )
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY env var"]
async fn real_api_simple_create() {
    let Some(client) = real_client() else {
        eprintln!("ANTHROPIC_API_KEY not set, skipping");
        return;
    };

    let request = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(64)
        .messages(vec![MessageParam::user("Reply with just: pong")])
        .build()
        .expect("build should succeed");

    let msg = client
        .messages()
        .create(&request)
        .await
        .expect("create should succeed against real API");

    assert_eq!(
        msg.role,
        sylvander_llm_anthropic::api::types::MessageRole::Assistant
    );
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
    assert!(msg.usage.input_tokens > 0);
    assert!(msg.usage.output_tokens > 0);
    assert!(!msg.content.is_empty());
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY env var"]
async fn real_api_streaming_assembly() {
    let Some(client) = real_client() else {
        eprintln!("ANTHROPIC_API_KEY not set, skipping");
        return;
    };

    let request = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(128)
        .messages(vec![MessageParam::user("Count from 1 to 5")])
        .build()
        .expect("build should succeed");

    let mut stream = client
        .messages()
        .stream(&request)
        .await
        .expect("stream should succeed");

    let mut saw_message_stop = false;
    let mut text_deltas = 0;
    while let Some(event) = stream.next().await {
        match event.expect("event should be Ok") {
            RawStreamEvent::ContentBlockDelta { .. } => text_deltas += 1,
            RawStreamEvent::MessageStop => saw_message_stop = true,
            _ => {}
        }
    }
    assert!(saw_message_stop, "expected to see MessageStop");
    assert!(text_deltas > 0, "expected at least one text delta");

    let final_msg = stream.final_message().expect("final_message");
    assert_eq!(final_msg.stop_reason, Some(StopReason::EndTurn));
    assert!(!final_msg.content.is_empty());
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY env var"]
async fn real_api_connection_reuse_multiple_requests() {
    let Some(client) = real_client() else {
        eprintln!("ANTHROPIC_API_KEY not set, skipping");
        return;
    };

    // Send 3 sequential requests to verify connection pool reuse
    let request = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(32)
        .messages(vec![MessageParam::user("hi")])
        .build()
        .expect("build should succeed");

    for i in 0..3 {
        let msg = client
            .messages()
            .create(&request)
            .await
            .unwrap_or_else(|e| panic!("request {i} failed: {e}"));
        assert!(msg.usage.input_tokens > 0);
    }
}
