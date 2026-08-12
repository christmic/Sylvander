//! `OpenAI` Responses and Chat Completions protocol adapters.
//!
//! The caller supplies the endpoint, credential, protocol, and provider feature
//! switches explicitly. This crate never reads process environment variables.

mod convert;
mod stream;

use std::collections::BTreeSet;
use std::sync::Arc;

use reqwest::Url;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use sylvander_llm_core::{
    ModelProvider, ModelRequest, ProviderError, ProviderErrorKind, ProviderErrorPhase,
    ProviderFuture,
};

/// OpenAI-family HTTP protocol selected for one provider definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiProtocol {
    /// `POST /v1/responses`.
    Responses,
    /// `POST /v1/chat/completions`.
    ChatCompletions,
}

/// Explicit provider extensions allowed on top of an `OpenAI` protocol.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderFeatures {
    enabled: BTreeSet<String>,
}

impl ProviderFeatures {
    #[must_use]
    /// Construct feature switches from registry-owned names.
    pub fn new(values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            enabled: values.into_iter().map(Into::into).collect(),
        }
    }

    #[must_use]
    /// Test whether one explicitly configured extension is enabled.
    pub fn contains(&self, value: &str) -> bool {
        self.enabled.contains(value)
    }
}

/// Explicit connection and protocol configuration.
#[derive(Clone)]
pub struct OpenAiProviderConfig {
    /// Provider identifier used in qualified model references.
    pub provider_id: String,
    /// Base URL supplied by runtime configuration.
    pub base_url: Url,
    /// API credential supplied by a request-scoped lease.
    pub api_key: String,
    /// Selected wire protocol.
    pub protocol: OpenAiProtocol,
    /// Provider extension switches.
    pub features: ProviderFeatures,
}

/// HTTP adapter for one explicitly configured OpenAI-family provider.
#[derive(Clone)]
pub struct OpenAiProvider {
    config: Arc<OpenAiProviderConfig>,
    http: reqwest::Client,
}

impl OpenAiProvider {
    /// Construct a provider without consulting environment variables.
    pub fn new(config: OpenAiProviderConfig) -> Result<Self, ProviderError> {
        if config.api_key.is_empty() {
            return Err(error(
                ProviderErrorKind::InvalidRequest,
                ProviderErrorPhase::Open,
                "provider credential is empty",
            ));
        }
        Ok(Self {
            config: Arc::new(config),
            http: reqwest::Client::new(),
        })
    }

    fn endpoint(&self) -> Result<Url, ProviderError> {
        let path = match self.config.protocol {
            OpenAiProtocol::Responses => "v1/responses",
            OpenAiProtocol::ChatCompletions => "v1/chat/completions",
        };
        self.config.base_url.join(path).map_err(|_| {
            error(
                ProviderErrorKind::InvalidRequest,
                ProviderErrorPhase::Open,
                "provider endpoint is invalid",
            )
        })
    }

    fn headers(&self) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        let authorization = HeaderValue::from_str(&format!("Bearer {}", self.config.api_key))
            .map_err(|_| {
                error(
                    ProviderErrorKind::InvalidRequest,
                    ProviderErrorPhase::Open,
                    "provider credential is invalid",
                )
            })?;
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiProvider")
            .field("provider_id", &self.config.provider_id)
            .field("base_url", &self.config.base_url)
            .field("protocol", &self.config.protocol)
            .field("features", &self.config.features)
            .finish_non_exhaustive()
    }
}

impl ModelProvider for OpenAiProvider {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
        Box::pin(async move {
            if request.model.provider != self.config.provider_id {
                return Err(error(
                    ProviderErrorKind::InvalidRequest,
                    ProviderErrorPhase::Open,
                    "model provider does not match adapter",
                ));
            }
            let body = convert::request(self.config.protocol, &self.config.features, &request)?;
            let response = self
                .http
                .post(self.endpoint()?)
                .headers(self.headers()?)
                .json(&body)
                .send()
                .await
                .map_err(|source| transport(source, ProviderErrorPhase::Open))?;
            if !response.status().is_success() {
                return Err(http_status(response.status().as_u16()));
            }
            Ok(stream::events(
                response,
                self.config.protocol,
                self.config.provider_id.clone(),
                request.model,
            ))
        })
    }
}

fn error(
    kind: ProviderErrorKind,
    phase: ProviderErrorPhase,
    message: &'static str,
) -> ProviderError {
    ProviderError::new(kind, phase, message)
}

fn transport(source: reqwest::Error, phase: ProviderErrorPhase) -> ProviderError {
    let kind = if source.is_timeout() {
        ProviderErrorKind::Timeout
    } else {
        ProviderErrorKind::Transport
    };
    error(kind, phase, "model provider transport failed")
}

fn http_status(status: u16) -> ProviderError {
    let kind = match status {
        401 => ProviderErrorKind::Authentication,
        403 => ProviderErrorKind::PermissionDenied,
        404 => ProviderErrorKind::ModelNotFound,
        429 => ProviderErrorKind::RateLimited,
        500..=u16::MAX => ProviderErrorKind::Unavailable,
        _ => ProviderErrorKind::InvalidRequest,
    };
    let mut value = error(
        kind,
        ProviderErrorPhase::Open,
        "model provider rejected the request",
    );
    value.status = Some(status);
    value
}
