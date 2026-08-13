//! Credential-gated execution through provider-neutral production adapters.

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt as _;
use sylvander_agent::prelude::{
    AgentEvent, AgentExecutionPorts, AgentLoop, AgentLoopError, AgentTurnRequest,
    ConversationSnapshot, ToolRegistry,
};
use sylvander_agent::tool_context::defaults::system_tool_context;
use sylvander_agent::tool_invocation::{RegistryBoundToolGateway, ToolInvocationGateway};
use sylvander_llm_anthropic::AnthropicProvider;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::error::AnthropicError;
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::MessageParam;
use sylvander_llm_core::{
    CacheHint, ChatMessage, ModelCapabilities, ModelInfo, ModelProvider, ModelRef, ModelRequest,
    ModelResponse, ModelStreamEvent, ProviderError, ProviderErrorKind, ProviderErrorPhase,
    StopReason, SystemInstruction, TokenUsage,
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
    pub max_retries: u32,
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
    let provider = match build_provider(binding, credential.clone(), limits.request_timeout) {
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
                matches!(
                    binding.protocol.as_str(),
                    "anthropic_messages" | "anthropic_compatible"
                ),
            )
            .await
        }
        BenchScenario::RemoteTokenCount => {
            run_remote_token_count(binding, cell, &credential, limits).await
        }
        BenchScenario::OpenTimeout => {
            run_expected_open_timeout(provider.as_ref(), cell, limits.max_output_tokens).await
        }
        BenchScenario::TruncatedStream => {
            run_expected_truncated_stream(provider.as_ref(), cell, limits.max_output_tokens).await
        }
        BenchScenario::TransientRetry => run_expected_transient_retry(provider, cell, limits).await,
        BenchScenario::ProcessInterruption => Err(ProviderError::new(
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

async fn run_expected_open_timeout(
    provider: &dyn ModelProvider,
    cell: &MatrixCell,
    max_output_tokens: u32,
) -> Result<PassMetrics, ProviderError> {
    match run_single(provider, cell, max_output_tokens).await {
        Err(error)
            if error.kind == ProviderErrorKind::Timeout
                && error.phase == ProviderErrorPhase::Open =>
        {
            Ok(PassMetrics {
                attempts: 1,
                ..PassMetrics::default()
            })
        }
        Err(error) => Err(error),
        Ok(_) => Err(protocol_error("open-timeout fault unexpectedly completed")),
    }
}

async fn run_expected_truncated_stream(
    provider: &dyn ModelProvider,
    cell: &MatrixCell,
    max_output_tokens: u32,
) -> Result<PassMetrics, ProviderError> {
    match run_single(provider, cell, max_output_tokens).await {
        Err(error)
            if matches!(
                error.kind,
                ProviderErrorKind::Protocol | ProviderErrorKind::Transport
            ) && error.phase == ProviderErrorPhase::Stream =>
        {
            Ok(PassMetrics {
                attempts: 1,
                ..PassMetrics::default()
            })
        }
        Err(error) => Err(error),
        Ok(_) => Err(protocol_error(
            "truncated-stream fault unexpectedly completed",
        )),
    }
}

async fn run_expected_transient_retry(
    provider: Arc<dyn ModelProvider>,
    cell: &MatrixCell,
    limits: LiveLimits,
) -> Result<PassMetrics, ProviderError> {
    let tools = ToolRegistry::new();
    let tool_context = system_tool_context();
    let gateway = RegistryBoundToolGateway::new(tools.invocation_descriptors());
    let request = AgentTurnRequest {
        conversation: ConversationSnapshot::new(vec![ChatMessage::user("Reply only: recovered")]),
        model: ModelInfo {
            reference: ModelRef::new(&cell.coordinate.provider_id, &cell.coordinate.model_id),
            context_window: 128_000,
            max_output_tokens: limits.max_output_tokens,
            capabilities: ModelCapabilities::empty(),
        },
        system_instructions: Vec::new(),
        reasoning: None,
        tools: tools.clone(),
        execution: tool_context.execution.as_ref().clone(),
    };
    let ports =
        AgentExecutionPorts::new(provider, tool_context, gateway.clone(), gateway.snapshot());
    let kernel = AgentLoop::builder()
        .max_iterations(1)
        .max_retries(limits.max_retries)
        .build();
    let mut retries = 0_u32;
    let outcome = sylvander_agent::loop_::run_with_events(&kernel, request, ports, |event| {
        if matches!(event, AgentEvent::ModelRetry { .. }) {
            retries = retries.saturating_add(1);
        }
    })
    .await
    .map_err(agent_error)?;
    if retries != limits.max_retries || outcome.final_response.text().is_empty() {
        return Err(protocol_error(
            "transient fault did not consume the exact retry budget",
        ));
    }
    Ok(metrics(
        outcome.final_response.usage,
        retries.saturating_add(1),
    ))
}

fn agent_error(error: AgentLoopError) -> ProviderError {
    match error {
        AgentLoopError::Provider { source, .. } => source,
        _ => ProviderError::new(
            ProviderErrorKind::Other,
            ProviderErrorPhase::Open,
            "Agent retry harness failed",
        ),
    }
}

async fn run_remote_token_count(
    binding: &ProtocolBinding,
    cell: &MatrixCell,
    credential: &str,
    limits: LiveLimits,
) -> Result<PassMetrics, ProviderError> {
    if !matches!(
        binding.protocol.as_str(),
        "anthropic_messages" | "anthropic_compatible"
    ) {
        return Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            ProviderErrorPhase::Open,
            "selected protocol has no remote token-count operation",
        ));
    }
    let client = AnthropicClient::builder()
        .api_key(credential)
        .base_url(&binding.base_url)
        .timeout(limits.request_timeout)
        .build()
        .map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                ProviderErrorPhase::Open,
                "invalid Anthropic bench configuration",
            )
        })?;
    let request = CreateMessageRequest::builder()
        .model(&cell.coordinate.model_id)
        .max_tokens(limits.max_output_tokens)
        .messages(vec![MessageParam::user(
            "Count this request without generating",
        )])
        .build()
        .map_err(|_| {
            ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                ProviderErrorPhase::Open,
                "invalid token-count bench request",
            )
        })?;
    let count = client
        .messages()
        .count_tokens(&request)
        .await
        .map_err(normalize_anthropic_error)?;
    if count.input_tokens == 0 {
        return Err(protocol_error("remote token count was zero"));
    }
    Ok(PassMetrics {
        attempts: 1,
        counted_input_tokens: Some(u64::from(count.input_tokens)),
        ..PassMetrics::default()
    })
}

fn normalize_anthropic_error(error: AnthropicError) -> ProviderError {
    let kind = match error {
        AnthropicError::Http(ref source) if source.is_timeout() => ProviderErrorKind::Timeout,
        AnthropicError::Http(_) => ProviderErrorKind::Transport,
        AnthropicError::Api { status: 401, .. } => ProviderErrorKind::Authentication,
        AnthropicError::Api { status: 402, .. } => ProviderErrorKind::QuotaExceeded,
        AnthropicError::Api { status: 403, .. } => ProviderErrorKind::PermissionDenied,
        AnthropicError::Api { status: 404, .. } => ProviderErrorKind::ModelNotFound,
        AnthropicError::Api { status: 429, .. } => ProviderErrorKind::RateLimited,
        AnthropicError::Api { status, .. } if status >= 500 => ProviderErrorKind::Unavailable,
        AnthropicError::Api { .. } | AnthropicError::Validation(_) => {
            ProviderErrorKind::InvalidRequest
        }
        AnthropicError::Json(_)
        | AnthropicError::SseParse { .. }
        | AnthropicError::UnknownBlockType(_)
        | AnthropicError::UnknownStreamEventType(_) => ProviderErrorKind::Protocol,
    };
    ProviderError::new(
        kind,
        ProviderErrorPhase::Open,
        "Anthropic token-count operation failed",
    )
}

fn build_provider(
    binding: &ProtocolBinding,
    credential: String,
    timeout: Duration,
) -> Result<Arc<dyn ModelProvider>, &'static str> {
    match binding.protocol.as_str() {
        "anthropic_messages" | "anthropic_compatible" => {
            let client = AnthropicClient::builder()
                .api_key(credential)
                .base_url(&binding.base_url)
                .timeout(timeout)
                .build()
                .map_err(|_| "invalid_provider_configuration")?;
            Ok(Arc::new(AnthropicProvider::new(
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
                    features: ProviderFeatures::new(binding.provider_features.iter().cloned()),
                },
                timeout,
            )
            .map_err(|_| "invalid_provider_configuration")?;
            Ok(Arc::new(provider))
        }
        "dashscope_generation" => {
            let provider = DashScopeProvider::new_with_timeout(
                DashScopeProviderConfig {
                    provider_id: binding.provider_id.clone(),
                    base_url: parse_url(&binding.base_url)?,
                    api_key: credential,
                    features: DashScopeFeatures::new(binding.provider_features.iter().cloned()),
                },
                timeout,
            )
            .map_err(|_| "invalid_provider_configuration")?;
            Ok(Arc::new(provider))
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
    if response.stop_reason == StopReason::MaxOutputTokens {
        return Err(protocol_error(
            "completion exhausted the output-token limit",
        ));
    }
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
        ProviderErrorKind::QuotaExceeded => "quota_exceeded",
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
