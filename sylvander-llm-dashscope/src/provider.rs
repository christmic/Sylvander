//! Provider-neutral adapter over the typed native Generation SDK.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::Url;
use sylvander_llm_core::{
    ModelProvider, ModelRequest, ModelStreamEvent, ProviderError, ProviderErrorKind,
    ProviderErrorPhase, ProviderFuture,
};

use crate::api::{DEFAULT_TIMEOUT, DashScopeClient, GenerationStreamEvent};
use crate::convert;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashScopeProtocol {
    TextGeneration,
    MultimodalGeneration,
}

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

    fn is_valid(&self) -> bool {
        const ALLOWED: &[&str] = &[
            "enable_thinking",
            "thinking_budget",
            "parallel_tool_calls",
            "reasoning_content",
        ];
        self.enabled
            .iter()
            .all(|feature| ALLOWED.contains(&feature.as_str()))
    }
}

#[derive(Clone)]
pub struct DashScopeProviderConfig {
    pub provider_id: String,
    pub base_url: Url,
    pub api_key: String,
    pub protocol: DashScopeProtocol,
    pub features: DashScopeFeatures,
}

#[derive(Clone)]
pub struct DashScopeProvider {
    provider_id: Arc<str>,
    features: DashScopeFeatures,
    protocol: DashScopeProtocol,
    client: DashScopeClient,
}

impl DashScopeProvider {
    pub fn new(config: DashScopeProviderConfig) -> Result<Self, ProviderError> {
        Self::new_with_timeout(config, DEFAULT_TIMEOUT)
    }

    pub fn new_with_timeout(
        config: DashScopeProviderConfig,
        timeout: Duration,
    ) -> Result<Self, ProviderError> {
        if !config.features.is_valid() {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                ProviderErrorPhase::Open,
                "provider feature is unsupported by native Generation",
            ));
        }
        let endpoint = match config.protocol {
            DashScopeProtocol::TextGeneration => "api/v1/services/aigc/text-generation/generation",
            DashScopeProtocol::MultimodalGeneration => {
                "api/v1/services/aigc/multimodal-generation/generation"
            }
        };
        let client =
            DashScopeClient::new_with_endpoint(config.base_url, &config.api_key, timeout, endpoint)
                .map_err(|error| convert::error(error, ProviderErrorPhase::Open))?;
        Ok(Self {
            provider_id: config.provider_id.into(),
            features: config.features,
            protocol: config.protocol,
            client,
        })
    }
}

impl std::fmt::Debug for DashScopeProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashScopeProvider")
            .field("provider_id", &self.provider_id)
            .field("features", &self.features)
            .field("protocol", &self.protocol)
            .finish_non_exhaustive()
    }
}

impl ModelProvider for DashScopeProvider {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
        Box::pin(async move {
            if request.model.provider != self.provider_id.as_ref() {
                return Err(ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    ProviderErrorPhase::Open,
                    "model provider does not match adapter",
                ));
            }
            let model = request.model.model.clone();
            let stream = match self.protocol {
                DashScopeProtocol::TextGeneration => {
                    let wire = convert::request(&self.features, &request)?;
                    self.client.generation_stream(&wire).await
                }
                DashScopeProtocol::MultimodalGeneration => {
                    let wire = convert::multimodal_request(&self.features, &request)?;
                    self.client.generation_stream(&wire).await
                }
            }
            .map_err(|error| convert::error(error, ProviderErrorPhase::Open))?;
            let provider = self.provider_id.to_string();
            Ok(Box::pin(stream.map(move |event| {
                let event =
                    event.map_err(|error| convert::error(error, ProviderErrorPhase::Stream))?;
                match event {
                    GenerationStreamEvent::TextDelta(text) => Ok(ModelStreamEvent::TextDelta(text)),
                    GenerationStreamEvent::ReasoningDelta(text) => {
                        Ok(ModelStreamEvent::ReasoningDelta(text))
                    }
                    GenerationStreamEvent::Completed(response) => Ok(ModelStreamEvent::Completed(
                        Box::new(convert::response(&provider, &model, response)?),
                    )),
                }
            })) as sylvander_llm_core::ModelEventStream)
        })
    }
}
