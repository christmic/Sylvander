//! Native `DashScope` Generation protocol adapter.
//!
//! Endpoint, credential, and provider feature switches are explicit constructor
//! inputs. This crate never reads process environment variables.

mod convert;
mod stream;

use std::collections::BTreeSet;
use std::sync::Arc;

use reqwest::Url;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use sylvander_llm_core::{
    ModelProvider, ModelRequest, ProviderError, ProviderErrorKind, ProviderErrorPhase,
    ProviderFuture,
};

/// Explicit native-protocol extensions enabled for one provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DashScopeFeatures {
    enabled: BTreeSet<String>,
}

impl DashScopeFeatures {
    #[must_use]
    pub fn new(values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            enabled: values.into_iter().map(Into::into).collect(),
        }
    }

    #[must_use]
    pub fn contains(&self, value: &str) -> bool {
        self.enabled.contains(value)
    }
}

/// Explicit native `DashScope` provider configuration.
#[derive(Clone)]
pub struct DashScopeProviderConfig {
    pub provider_id: String,
    pub base_url: Url,
    pub api_key: String,
    pub features: DashScopeFeatures,
}

/// Native `text-generation/generation` provider adapter.
#[derive(Clone)]
pub struct DashScopeProvider {
    config: Arc<DashScopeProviderConfig>,
    http: reqwest::Client,
}

impl DashScopeProvider {
    pub fn new(config: DashScopeProviderConfig) -> Result<Self, ProviderError> {
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
        self.config
            .base_url
            .join("api/v1/services/aigc/text-generation/generation")
            .map_err(|_| {
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
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert("x-dashscope-sse", HeaderValue::from_static("enable"));
        Ok(headers)
    }
}

impl std::fmt::Debug for DashScopeProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashScopeProvider")
            .field("provider_id", &self.config.provider_id)
            .field("base_url", &self.config.base_url)
            .field("features", &self.config.features)
            .finish_non_exhaustive()
    }
}

impl ModelProvider for DashScopeProvider {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
        Box::pin(async move {
            if request.model.provider != self.config.provider_id {
                return Err(error(
                    ProviderErrorKind::InvalidRequest,
                    ProviderErrorPhase::Open,
                    "model provider does not match adapter",
                ));
            }
            let body = convert::request(&self.config.features, &request)?;
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
