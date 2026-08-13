//! Credential-gated conformance tests against an Anthropic Messages endpoint.
//!
//! These ignored tests fail when explicitly selected without complete bench
//! configuration. They never treat a missing credential as a passing skip.
//!
//! ```bash
//! SYLVANDER_BENCH_ANTHROPIC_API_KEY=... \
//! SYLVANDER_BENCH_ANTHROPIC_BASE_URL=https://api.anthropic.com \
//! SYLVANDER_BENCH_ANTHROPIC_MODEL=... \
//! cargo test -p sylvander-llm-anthropic --test real_api -- --ignored
//! ```

use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt as _;
use serde_json::json;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::{
    CacheControl, MessageParam, RawStreamEvent, StopReason, SystemBlock, SystemPrompt,
    SystemTextBlock, Usage,
};

const PROVIDER: &str = "anthropic";
const CACHE_PREFIX_CHARS: usize = 24_000;

struct LiveConfig {
    client: AnthropicClient,
    model: String,
    endpoint_origin: String,
}

fn required_env(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("required live bench configuration {name} is missing"))
}

fn live_config() -> LiveConfig {
    let api_key = required_env("SYLVANDER_BENCH_ANTHROPIC_API_KEY");
    let base_url = required_env("SYLVANDER_BENCH_ANTHROPIC_BASE_URL");
    let model = required_env("SYLVANDER_BENCH_ANTHROPIC_MODEL");
    let client = AnthropicClient::builder()
        .api_key(api_key)
        .base_url(base_url)
        .timeout(Duration::from_mins(1))
        .build()
        .expect("explicit live client configuration must be valid");
    let url = client.base_url();
    let endpoint_origin = format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str().expect("bench URL must have a host"),
        url.port()
            .map_or_else(String::new, |port| format!(":{port}"))
    );
    LiveConfig {
        client,
        model,
        endpoint_origin,
    }
}

fn request(model: &str, prompt: &str, max_tokens: u32) -> CreateMessageRequest {
    CreateMessageRequest::builder()
        .model(model)
        .max_tokens(max_tokens)
        .messages(vec![MessageParam::user(prompt)])
        .build()
        .expect("fixed live request must be valid")
}

fn cache_request(model: &str, prompt: &str) -> CreateMessageRequest {
    let prefix = "Stable Sylvander cache conformance context. ".repeat(600);
    assert!(prefix.len() >= CACHE_PREFIX_CHARS);
    CreateMessageRequest::builder()
        .model(model)
        .max_tokens(16)
        .system(SystemPrompt::Blocks(vec![SystemBlock::Text(
            SystemTextBlock::new(prefix).with_cache_control(CacheControl::ephemeral()),
        )]))
        .messages(vec![MessageParam::user(prompt)])
        .build()
        .expect("fixed cache request must be valid")
}

fn git_output(args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .output()
        .expect("git must be available to identify live evidence");
    assert!(output.status.success(), "git evidence query must succeed");
    String::from_utf8(output.stdout)
        .expect("git output must be UTF-8")
        .trim()
        .to_owned()
}

fn now_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must follow the Unix epoch")
        .as_millis()
}

fn emit(
    case_id: &str,
    config: &LiveConfig,
    started_at: u128,
    duration: Duration,
    usage: &Usage,
    counted_input_tokens: Option<u32>,
) {
    eprintln!(
        "{}",
        json!({
            "schema_version": 1,
            "run_id": format!("anthropic-{case_id}-{started_at}"),
            "case_id": case_id,
            "case_revision": 1,
            "status": "passed",
            "sylvander_commit": git_output(&["rev-parse", "HEAD"]),
            "worktree_dirty": !git_output(&["status", "--porcelain"]).is_empty(),
            "provider_id": PROVIDER,
            "protocol": "anthropic_messages",
            "model_id": config.model,
            "endpoint_origin": config.endpoint_origin,
            "started_at_unix_ms": started_at,
            "duration_ms": u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            "attempts": 1,
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "cache_write_tokens": usage.cache_creation_input_tokens,
            "cache_read_tokens": usage.cache_read_input_tokens,
            "reasoning_tokens": usage.output_tokens_details.map(|value| value.thinking_tokens),
            "counted_input_tokens": counted_input_tokens,
        })
    );
}

#[tokio::test]
#[ignore = "requires explicit Anthropic live bench configuration"]
async fn real_api_simple_create() {
    let config = live_config();
    let started_at = now_unix_millis();
    let started = Instant::now();
    let message = config
        .client
        .messages()
        .create(&request(&config.model, "Reply with just: pong", 16))
        .await
        .expect("live create must succeed");

    assert_eq!(message.stop_reason, Some(StopReason::EndTurn));
    assert!(message.usage.input_tokens > 0);
    assert!(message.usage.output_tokens > 0);
    assert!(!message.content.is_empty());
    emit(
        "connectivity_usage",
        &config,
        started_at,
        started.elapsed(),
        &message.usage,
        None,
    );
}

#[tokio::test]
#[ignore = "requires explicit Anthropic live bench configuration"]
async fn real_api_streaming_assembly() {
    let config = live_config();
    let started_at = now_unix_millis();
    let started = Instant::now();
    let request = request(&config.model, "Reply with just: stream-pong", 16);
    let mut stream = config
        .client
        .messages()
        .stream(&request)
        .await
        .expect("live stream must open");
    let mut saw_stop = false;
    let mut text_deltas = 0_u64;
    while let Some(event) = stream.next().await {
        match event.expect("live stream event must be valid") {
            RawStreamEvent::ContentBlockDelta { .. } => text_deltas += 1,
            RawStreamEvent::MessageStop => saw_stop = true,
            _ => {}
        }
    }
    let message = stream
        .final_message()
        .expect("stream must assemble a message");
    assert!(saw_stop);
    assert!(text_deltas > 0);
    assert_eq!(message.stop_reason, Some(StopReason::EndTurn));
    assert!(message.usage.input_tokens > 0);
    assert!(message.usage.output_tokens > 0);
    emit(
        "streaming_usage",
        &config,
        started_at,
        started.elapsed(),
        &message.usage,
        None,
    );
}

#[tokio::test]
#[ignore = "requires explicit Anthropic live bench configuration"]
async fn real_api_remote_token_count() {
    let config = live_config();
    let started_at = now_unix_millis();
    let started = Instant::now();
    let count = config
        .client
        .messages()
        .count_tokens(&request(
            &config.model,
            "Count this request without generating",
            16,
        ))
        .await
        .expect("live token count must succeed");
    assert!(count.input_tokens > 0);
    emit(
        "remote_token_count",
        &config,
        started_at,
        started.elapsed(),
        &Usage::default(),
        Some(count.input_tokens),
    );
}

#[tokio::test]
#[ignore = "requires an Anthropic model with explicit prompt caching"]
async fn real_api_prompt_cache_write_then_read() {
    let config = live_config();
    let started_at = now_unix_millis();
    let started = Instant::now();
    let first = config
        .client
        .messages()
        .create(&cache_request(&config.model, "Reply only: first"))
        .await
        .expect("cache-creation request must succeed");
    let second = config
        .client
        .messages()
        .create(&cache_request(&config.model, "Reply only: second"))
        .await
        .expect("cache-read request must succeed");
    assert!(first.usage.cache_creation_input_tokens.unwrap_or(0) > 0);
    assert!(second.usage.cache_read_input_tokens.unwrap_or(0) > 0);
    let combined = Usage {
        input_tokens: first
            .usage
            .input_tokens
            .saturating_add(second.usage.input_tokens),
        output_tokens: first
            .usage
            .output_tokens
            .saturating_add(second.usage.output_tokens),
        cache_creation_input_tokens: first.usage.cache_creation_input_tokens,
        cache_read_input_tokens: second.usage.cache_read_input_tokens,
        ..Usage::default()
    };
    emit(
        "cache_write_read",
        &config,
        started_at,
        started.elapsed(),
        &combined,
        None,
    );
}
