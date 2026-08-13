//! Production-provider construction for external Agent benchmark deployments.

use std::sync::Arc;
use std::time::Duration;

use sylvander_llm_anthropic::AnthropicProvider;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_core::ModelProvider;
use sylvander_llm_dashscope::{
    DashScopeFeatures, DashScopeProtocol, DashScopeProvider, DashScopeProviderConfig,
};
use sylvander_llm_openai::{
    OpenAiProtocol, OpenAiProvider, OpenAiProviderConfig, ProviderFeatures,
};
use url::Url;

/// Protocol profile selected by one provider deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentProtocol {
    AnthropicMessages,
    OpenAiResponses,
    OpenAiChatCompletions,
    DashScopeGeneration,
}

impl AgentProtocol {
    /// Parse the stable protocol identifier used in benchmark coordinates.
    pub fn parse(value: &str) -> Result<Self, ProviderBuildError> {
        match value {
            "anthropic_messages" | "anthropic_compatible" => Ok(Self::AnthropicMessages),
            "openai_responses" => Ok(Self::OpenAiResponses),
            "openai_chat_completions" => Ok(Self::OpenAiChatCompletions),
            "dashscope_generation" => Ok(Self::DashScopeGeneration),
            _ => Err(ProviderBuildError::UnsupportedProtocol),
        }
    }
}

/// Provider-owned endpoint and feature profile for an Agent benchmark run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProviderBinding {
    pub provider_id: String,
    pub protocol: AgentProtocol,
    pub base_url: String,
    pub provider_features: Vec<String>,
}

/// Content-safe provider construction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProviderBuildError {
    #[error("unsupported protocol")]
    UnsupportedProtocol,
    #[error("invalid provider configuration")]
    InvalidConfiguration,
}

/// Construct the production protocol adapter selected by one deployment.
pub fn build_provider(
    binding: &AgentProviderBinding,
    credential: String,
    timeout: Duration,
) -> Result<Arc<dyn ModelProvider>, ProviderBuildError> {
    match binding.protocol {
        AgentProtocol::AnthropicMessages => {
            if !binding.provider_features.is_empty() {
                return Err(ProviderBuildError::InvalidConfiguration);
            }
            let client = AnthropicClient::builder()
                .api_key(credential)
                .base_url(&binding.base_url)
                .timeout(timeout)
                .build()
                .map_err(|_| ProviderBuildError::InvalidConfiguration)?;
            Ok(Arc::new(AnthropicProvider::new(
                &binding.provider_id,
                client,
            )))
        }
        AgentProtocol::OpenAiResponses | AgentProtocol::OpenAiChatCompletions => {
            let protocol = match binding.protocol {
                AgentProtocol::OpenAiResponses => OpenAiProtocol::Responses,
                _ => OpenAiProtocol::ChatCompletions,
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
            .map_err(|_| ProviderBuildError::InvalidConfiguration)?;
            Ok(Arc::new(provider))
        }
        AgentProtocol::DashScopeGeneration => {
            let provider = DashScopeProvider::new_with_timeout(
                DashScopeProviderConfig {
                    provider_id: binding.provider_id.clone(),
                    base_url: parse_url(&binding.base_url)?,
                    api_key: credential,
                    protocol: DashScopeProtocol::TextGeneration,
                    features: DashScopeFeatures::new(binding.provider_features.iter().cloned()),
                },
                timeout,
            )
            .map_err(|_| ProviderBuildError::InvalidConfiguration)?;
            Ok(Arc::new(provider))
        }
    }
}

fn parse_url(value: &str) -> Result<Url, ProviderBuildError> {
    Url::parse(value).map_err(|_| ProviderBuildError::InvalidConfiguration)
}
