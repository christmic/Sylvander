//! Provider-neutral adapter over the typed `OpenAI` SDK layers.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::Url;
use sylvander_llm_core::{
    ModelProvider, ModelRequest, ModelStreamEvent, ProviderError, ProviderErrorKind,
    ProviderErrorPhase, ProviderFuture,
};

use crate::api::chat::ChatStreamEvent;
use crate::api::responses::ResponseStreamEvent;
use crate::api::{DEFAULT_TIMEOUT, OpenAiClient};
use crate::convert;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiProtocol {
    Responses,
    ChatCompletions,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderFeatures {
    enabled: BTreeSet<String>,
}

impl ProviderFeatures {
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

    fn valid_for(&self, protocol: OpenAiProtocol) -> bool {
        let allowed: &[&str] = match protocol {
            OpenAiProtocol::Responses => &["enable_thinking"],
            OpenAiProtocol::ChatCompletions => &[
                "enable_thinking",
                "max_completion_tokens",
                "reasoning_content",
            ],
        };
        self.enabled
            .iter()
            .all(|feature| allowed.contains(&feature.as_str()))
    }
}

#[derive(Clone)]
pub struct OpenAiProviderConfig {
    pub provider_id: String,
    pub base_url: Url,
    pub api_key: String,
    pub protocol: OpenAiProtocol,
    pub features: ProviderFeatures,
}

#[derive(Clone)]
pub struct OpenAiProvider {
    provider_id: Arc<str>,
    protocol: OpenAiProtocol,
    features: ProviderFeatures,
    client: OpenAiClient,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiProviderConfig) -> Result<Self, ProviderError> {
        Self::new_with_timeout(config, DEFAULT_TIMEOUT)
    }

    pub fn new_with_timeout(
        config: OpenAiProviderConfig,
        timeout: Duration,
    ) -> Result<Self, ProviderError> {
        if !config.features.valid_for(config.protocol) {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                ProviderErrorPhase::Open,
                "provider feature is unsupported by selected OpenAI protocol",
            ));
        }
        let client = OpenAiClient::new_with_timeout(config.base_url, &config.api_key, timeout)
            .map_err(|error| convert::error(error, ProviderErrorPhase::Open))?;
        Ok(Self {
            provider_id: config.provider_id.into(),
            protocol: config.protocol,
            features: config.features,
            client,
        })
    }
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiProvider")
            .field("provider_id", &self.provider_id)
            .field("protocol", &self.protocol)
            .field("features", &self.features)
            .finish_non_exhaustive()
    }
}

impl ModelProvider for OpenAiProvider {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
        Box::pin(async move {
            if request.model.provider != self.provider_id.as_ref() {
                return Err(ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    ProviderErrorPhase::Open,
                    "model provider does not match adapter",
                ));
            }
            match self.protocol {
                OpenAiProtocol::Responses => self.responses(request).await,
                OpenAiProtocol::ChatCompletions => self.chat(request).await,
            }
        })
    }
}

impl OpenAiProvider {
    async fn responses(
        &self,
        request: ModelRequest,
    ) -> Result<sylvander_llm_core::ModelEventStream, ProviderError> {
        let wire = convert::responses_request(&self.features, &request)?;
        let stream = self
            .client
            .responses_stream(&wire)
            .await
            .map_err(|error| convert::error(error, ProviderErrorPhase::Open))?;
        let provider = self.provider_id.to_string();
        Ok(Box::pin(stream.map(move |event| {
            let event = event.map_err(|error| convert::error(error, ProviderErrorPhase::Stream))?;
            match event {
                ResponseStreamEvent::OutputTextDelta(text) => Ok(ModelStreamEvent::TextDelta(text)),
                ResponseStreamEvent::ReasoningDelta(text) => {
                    Ok(ModelStreamEvent::ReasoningDelta(text))
                }
                ResponseStreamEvent::Completed(response)
                | ResponseStreamEvent::Incomplete(response) => Ok(ModelStreamEvent::Completed(
                    Box::new(convert::responses_response(&provider, response)?),
                )),
            }
        })))
    }

    async fn chat(
        &self,
        request: ModelRequest,
    ) -> Result<sylvander_llm_core::ModelEventStream, ProviderError> {
        let wire = convert::chat_request(&self.features, &request)?;
        let stream = self
            .client
            .chat_completions_stream(&wire)
            .await
            .map_err(|error| convert::error(error, ProviderErrorPhase::Open))?;
        let provider = self.provider_id.to_string();
        Ok(Box::pin(stream.map(move |event| {
            let event = event.map_err(|error| convert::error(error, ProviderErrorPhase::Stream))?;
            match event {
                ChatStreamEvent::ContentDelta(text) => Ok(ModelStreamEvent::TextDelta(text)),
                ChatStreamEvent::ReasoningDelta(text) => Ok(ModelStreamEvent::ReasoningDelta(text)),
                ChatStreamEvent::Completed(response) => Ok(ModelStreamEvent::Completed(Box::new(
                    convert::chat_response(&provider, *response)?,
                ))),
            }
        })))
    }
}
