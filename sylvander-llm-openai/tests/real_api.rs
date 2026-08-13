//! Credential-gated conformance tests for `OpenAI` Responses and Chat Completions.
//!
//! Explicitly selected tests fail when configuration is incomplete.

use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt as _;
use reqwest::Url;
use serde_json::json;
use sylvander_llm_core::{
    ChatMessage, ModelProvider, ModelRef, ModelRequest, ModelResponse, ModelStreamEvent,
    SystemInstruction, TokenUsage,
};
use sylvander_llm_openai::{
    OpenAiProtocol, OpenAiProvider, OpenAiProviderConfig, ProviderFeatures,
};

const PROVIDER: &str = "openai";
const CACHE_PREFIX_CHARS: usize = 24_000;

struct BaseConfig {
    api_key: String,
    base_url: Url,
    endpoint_origin: String,
}

fn required_env(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("required live bench configuration {name} is missing"))
}

fn base_config() -> BaseConfig {
    let api_key = required_env("SYLVANDER_BENCH_OPENAI_API_KEY");
    let base_url = Url::parse(&required_env("SYLVANDER_BENCH_OPENAI_BASE_URL"))
        .expect("bench base URL must be valid");
    let endpoint_origin = format!(
        "{}://{}{}",
        base_url.scheme(),
        base_url.host_str().expect("bench URL must have a host"),
        base_url
            .port()
            .map_or_else(String::new, |port| format!(":{port}"))
    );
    BaseConfig {
        api_key,
        base_url,
        endpoint_origin,
    }
}

fn provider(config: &BaseConfig, protocol: OpenAiProtocol) -> OpenAiProvider {
    OpenAiProvider::new_with_timeout(
        OpenAiProviderConfig {
            provider_id: PROVIDER.into(),
            base_url: config.base_url.clone(),
            api_key: config.api_key.clone(),
            protocol,
            features: ProviderFeatures::default(),
        },
        Duration::from_mins(1),
    )
    .expect("explicit live provider configuration must be valid")
}

fn request(model: &str, system: Option<String>, prompt: &str) -> ModelRequest {
    ModelRequest {
        request_id: format!("live-{}", now_unix_millis()),
        model: ModelRef::new(PROVIDER, model),
        system: system
            .map(|text| SystemInstruction {
                text,
                cache_hint: None,
            })
            .into_iter()
            .collect(),
        messages: vec![ChatMessage::user(prompt)],
        tools: Vec::new(),
        max_output_tokens: 32,
        reasoning: None,
        output_schema: None,
    }
}

async fn complete(provider: &OpenAiProvider, request: ModelRequest) -> ModelResponse {
    let mut stream = provider
        .complete_stream(request)
        .await
        .expect("live stream must open");
    let mut completed = None;
    while let Some(event) = stream.next().await {
        if let ModelStreamEvent::Completed(response) = event.expect("live event must be valid") {
            assert!(completed.replace(*response).is_none());
        }
    }
    let response = completed.expect("live stream must complete exactly once");
    assert!(!response.text().is_empty());
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
    response
}

fn cache_prefix() -> String {
    let prefix = "Stable Sylvander OpenAI cache conformance context. ".repeat(560);
    assert!(prefix.len() >= CACHE_PREFIX_CHARS);
    prefix
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
    protocol: &str,
    model: &str,
    config: &BaseConfig,
    started_at: u128,
    duration: Duration,
    usage: TokenUsage,
) {
    eprintln!(
        "{}",
        json!({
            "schema_version": 1,
            "run_id": format!("openai-{case_id}-{started_at}"),
            "case_id": case_id,
            "case_revision": 1,
            "status": "passed",
            "sylvander_commit": git_output(&["rev-parse", "HEAD"]),
            "worktree_dirty": !git_output(&["status", "--porcelain"]).is_empty(),
            "provider_id": PROVIDER,
            "protocol": protocol,
            "model_id": model,
            "endpoint_origin": config.endpoint_origin,
            "started_at_unix_ms": started_at,
            "duration_ms": u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            "attempts": 1,
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "cache_write_tokens": usage.cache_write_tokens,
            "cache_read_tokens": usage.cache_read_tokens,
            "reasoning_tokens": usage.details.reasoning_tokens,
            "reported_total_tokens": usage.details.reported_total_tokens,
        })
    );
}

async fn connectivity_case(protocol: OpenAiProtocol, protocol_name: &str, model_env: &str) {
    let config = base_config();
    let model = required_env(model_env);
    let started_at = now_unix_millis();
    let started = Instant::now();
    let response = complete(
        &provider(&config, protocol),
        request(&model, None, "Reply with just: pong"),
    )
    .await;
    emit(
        "connectivity_usage",
        protocol_name,
        &model,
        &config,
        started_at,
        started.elapsed(),
        response.usage,
    );
}

async fn cache_case(protocol: OpenAiProtocol, protocol_name: &str, model_env: &str) {
    let config = base_config();
    let model = required_env(model_env);
    let provider = provider(&config, protocol);
    let prefix = cache_prefix();
    let started_at = now_unix_millis();
    let started = Instant::now();
    let first = complete(
        &provider,
        request(&model, Some(prefix.clone()), "Reply only: first"),
    )
    .await;
    let second = complete(
        &provider,
        request(&model, Some(prefix), "Reply only: second"),
    )
    .await;
    assert!(second.usage.cache_read_tokens.unwrap_or(0) > 0);
    let usage = TokenUsage {
        input_tokens: first
            .usage
            .input_tokens
            .saturating_add(second.usage.input_tokens),
        output_tokens: first
            .usage
            .output_tokens
            .saturating_add(second.usage.output_tokens),
        cache_write_tokens: first.usage.cache_write_tokens,
        cache_read_tokens: second.usage.cache_read_tokens,
        ..TokenUsage::default()
    };
    emit(
        "cache_write_read",
        protocol_name,
        &model,
        &config,
        started_at,
        started.elapsed(),
        usage,
    );
}

#[tokio::test]
#[ignore = "requires explicit OpenAI Responses live bench configuration"]
async fn real_responses_connectivity_and_usage() {
    connectivity_case(
        OpenAiProtocol::Responses,
        "openai_responses",
        "SYLVANDER_BENCH_OPENAI_RESPONSES_MODEL",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires an OpenAI Responses model with prompt caching"]
async fn real_responses_prompt_cache_hit() {
    cache_case(
        OpenAiProtocol::Responses,
        "openai_responses",
        "SYLVANDER_BENCH_OPENAI_RESPONSES_MODEL",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires explicit OpenAI Chat Completions live bench configuration"]
async fn real_chat_connectivity_and_usage() {
    connectivity_case(
        OpenAiProtocol::ChatCompletions,
        "openai_chat_completions",
        "SYLVANDER_BENCH_OPENAI_CHAT_MODEL",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires an OpenAI Chat Completions model with prompt caching"]
async fn real_chat_prompt_cache_hit() {
    cache_case(
        OpenAiProtocol::ChatCompletions,
        "openai_chat_completions",
        "SYLVANDER_BENCH_OPENAI_CHAT_MODEL",
    )
    .await;
}
