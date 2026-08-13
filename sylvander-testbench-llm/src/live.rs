//! Credential-gated execution through provider-neutral production adapters.

use std::env;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt as _;
use sylvander_llm_anthropic::AnthropicProvider;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_core::{
    CacheHint, ChatMessage, ModelProvider, ModelRef, ModelRequest, ModelResponse, ModelStreamEvent,
    ProviderError, ProviderErrorKind, ProviderErrorPhase, SystemInstruction, TokenUsage,
};
use sylvander_llm_dashscope::{DashScopeFeatures, DashScopeProvider, DashScopeProviderConfig};
use sylvander_llm_openai::{
    OpenAiProtocol, OpenAiProvider, OpenAiProviderConfig, ProviderFeatures,
};
use url::Url;

use crate::{
    Applicability, BenchObservation, BenchResult, BenchScenario, BenchStatus, MatrixCell,
    PassMetrics, ProtocolBinding, RepositoryState, endpoint_origin,
};

const CACHE_PREFIX_CHARS: usize = 24_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveLimits {
    pub request_timeout: Duration,
    pub max_output_tokens: u32,
}

pub async fn run_live_cell(
    binding: &ProtocolBinding,
    cell: &MatrixCell,
    limits: LiveLimits,
    repository: RepositoryState,
) -> BenchResult {
    let started_at = now_unix_millis();
    let started = Instant::now();
    let origin = Url::parse(&binding.base_url)
        .map(|url| endpoint_origin(&url))
        .unwrap_or_default();
    if cell.applicability != Applicability::Required {
        return record(
            cell,
            BenchStatus::NotApplicable,
            origin,
            started_at,
            started.elapsed(),
            repository,
            BenchObservation::default(),
        );
    }
    let Some(credential) = env::var(&binding.credential_env)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return record(
            cell,
            BenchStatus::NotRun,
            origin,
            started_at,
            started.elapsed(),
            repository,
            failure("missing_configuration", None),
        );
    };
    let provider = match build_provider(binding, credential, limits.request_timeout) {
        Ok(provider) => provider,
        Err(kind) => {
            return record(
                cell,
                BenchStatus::InfrastructureError,
                origin,
                started_at,
                started.elapsed(),
                repository,
                failure(kind, None),
            );
        }
    };
    let outcome = match cell.coordinate.scenario {
        BenchScenario::Connectivity | BenchScenario::Usage => {
            run_single(provider.as_ref(), cell, limits.max_output_tokens).await
        }
        BenchScenario::CacheWriteRead => {
            run_cache(
                provider.as_ref(),
                cell,
                limits.max_output_tokens,
                binding.protocol == "anthropic_messages",
            )
            .await
        }
        _ => Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            ProviderErrorPhase::Open,
            "scenario requires a dedicated bench harness",
        )),
    };
    let (status, observation) = match outcome {
        Ok(metrics) => (
            BenchStatus::Passed,
            BenchObservation {
                metrics,
                ..BenchObservation::default()
            },
        ),
        Err(error) => (
            BenchStatus::Failed,
            failure(error_kind(error.kind), Some(error_phase(error.phase))),
        ),
    };
    record(
        cell,
        status,
        origin,
        started_at,
        started.elapsed(),
        repository,
        observation,
    )
}

fn build_provider(
    binding: &ProtocolBinding,
    credential: String,
    timeout: Duration,
) -> Result<Box<dyn ModelProvider>, &'static str> {
    match binding.protocol.as_str() {
        "anthropic_messages" | "anthropic_compatible" => {
            let client = AnthropicClient::builder()
                .api_key(credential)
                .base_url(&binding.base_url)
                .timeout(timeout)
                .build()
                .map_err(|_| "invalid_provider_configuration")?;
            Ok(Box::new(AnthropicProvider::new(
                &binding.provider_id,
                client,
            )))
        }
        "openai_responses" | "openai_chat_completions" => {
            let protocol = if binding.protocol == "openai_responses" {
                OpenAiProtocol::Responses
            } else {
                OpenAiProtocol::ChatCompletions
            };
            let provider = OpenAiProvider::new_with_timeout(
                OpenAiProviderConfig {
                    provider_id: binding.provider_id.clone(),
                    base_url: parse_url(&binding.base_url)?,
                    api_key: credential,
                    protocol,
                    features: ProviderFeatures::default(),
                },
                timeout,
            )
            .map_err(|_| "invalid_provider_configuration")?;
            Ok(Box::new(provider))
        }
        "dashscope_generation" => {
            let provider = DashScopeProvider::new_with_timeout(
                DashScopeProviderConfig {
                    provider_id: binding.provider_id.clone(),
                    base_url: parse_url(&binding.base_url)?,
                    api_key: credential,
                    features: DashScopeFeatures::default(),
                },
                timeout,
            )
            .map_err(|_| "invalid_provider_configuration")?;
            Ok(Box::new(provider))
        }
        _ => Err("unsupported_protocol"),
    }
}

async fn run_single(
    provider: &dyn ModelProvider,
    cell: &MatrixCell,
    max_output_tokens: u32,
) -> Result<PassMetrics, ProviderError> {
    let response = complete(
        provider,
        request(cell, "Reply with just: pong", None, max_output_tokens),
    )
    .await?;
    if response.text().is_empty() {
        return Err(protocol_error("completion contained no user-visible text"));
    }
    if cell.coordinate.scenario == BenchScenario::Usage
        && (response.usage.input_tokens == 0 || response.usage.output_tokens == 0)
    {
        return Err(protocol_error("completion omitted required token usage"));
    }
    Ok(metrics(response.usage, 1))
}

async fn run_cache(
    provider: &dyn ModelProvider,
    cell: &MatrixCell,
    max_output_tokens: u32,
    explicit_cache_hint: bool,
) -> Result<PassMetrics, ProviderError> {
    let prefix = "Stable Sylvander cache benchmark context. ".repeat(600);
    if prefix.len() < CACHE_PREFIX_CHARS {
        return Err(protocol_error("cache prefix is below the declared bound"));
    }
    let cache_hint = explicit_cache_hint.then_some(CacheHint::Ephemeral);
    let first = complete(
        provider,
        request(
            cell,
            "Reply only: first",
            Some((prefix.clone(), cache_hint)),
            max_output_tokens,
        ),
    )
    .await?;
    let second = complete(
        provider,
        request(
            cell,
            "Reply only: second",
            Some((prefix, cache_hint)),
            max_output_tokens,
        ),
    )
    .await?;
    if second.usage.cache_read_tokens.unwrap_or(0) == 0 {
        return Err(protocol_error("repeated prefix reported no cache read"));
    }
    let mut usage = first.usage;
    usage.saturating_add_assign(second.usage);
    Ok(metrics(usage, 2))
}

async fn complete(
    provider: &dyn ModelProvider,
    request: ModelRequest,
) -> Result<ModelResponse, ProviderError> {
    let mut stream = provider.complete_stream(request).await?;
    let mut completed = None;
    while let Some(event) = stream.next().await {
        if let ModelStreamEvent::Completed(response) = event?
            && completed.replace(*response).is_some()
        {
            return Err(protocol_error("stream completed more than once"));
        }
    }
    completed.ok_or_else(|| protocol_error("stream ended before completion"))
}

fn request(
    cell: &MatrixCell,
    prompt: &str,
    system: Option<(String, Option<CacheHint>)>,
    max_output_tokens: u32,
) -> ModelRequest {
    ModelRequest {
        request_id: format!(
            "bench-{}-{}",
            cell.coordinate.scenario.as_str(),
            cell.coordinate.run_ordinal
        ),
        model: ModelRef::new(&cell.coordinate.provider_id, &cell.coordinate.model_id),
        system: system
            .map(|(text, cache_hint)| SystemInstruction { text, cache_hint })
            .into_iter()
            .collect(),
        messages: vec![ChatMessage::user(prompt)],
        tools: Vec::new(),
        max_output_tokens,
        reasoning: None,
        output_schema: None,
    }
}

fn metrics(usage: TokenUsage, attempts: u32) -> PassMetrics {
    PassMetrics {
        attempts,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        reasoning_tokens: usage.details.reasoning_tokens,
        reported_total_tokens: usage.details.reported_total_tokens,
        ..PassMetrics::default()
    }
}

fn protocol_error(message: &'static str) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Protocol,
        ProviderErrorPhase::Stream,
        message,
    )
}

fn parse_url(value: &str) -> Result<Url, &'static str> {
    Url::parse(value).map_err(|_| "invalid_provider_configuration")
}

fn record(
    cell: &MatrixCell,
    status: BenchStatus,
    origin: String,
    started_at: u64,
    duration: Duration,
    repository: RepositoryState,
    observation: BenchObservation,
) -> BenchResult {
    BenchResult::recorded(
        cell,
        1,
        status,
        origin,
        started_at,
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        repository,
        observation,
    )
}

fn failure(kind: &str, phase: Option<&str>) -> BenchObservation {
    BenchObservation {
        failure_kind: Some(kind.into()),
        failure_phase: phase.map(str::to_owned),
        ..BenchObservation::default()
    }
}

fn error_kind(kind: ProviderErrorKind) -> &'static str {
    match kind {
        ProviderErrorKind::Transport => "transport",
        ProviderErrorKind::Timeout => "timeout",
        ProviderErrorKind::RateLimited => "rate_limited",
        ProviderErrorKind::Authentication => "authentication",
        ProviderErrorKind::PermissionDenied => "permission_denied",
        ProviderErrorKind::ModelNotFound => "model_not_found",
        ProviderErrorKind::InvalidRequest => "invalid_request",
        ProviderErrorKind::Unsupported => "unsupported",
        ProviderErrorKind::Unavailable => "unavailable",
        ProviderErrorKind::Protocol => "protocol",
        ProviderErrorKind::Cancelled => "cancelled",
        ProviderErrorKind::Other => "other",
    }
}

const fn error_phase(phase: ProviderErrorPhase) -> &'static str {
    match phase {
        ProviderErrorPhase::Open => "open",
        ProviderErrorPhase::Stream => "stream",
    }
}

fn now_unix_millis() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must follow the Unix epoch")
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
