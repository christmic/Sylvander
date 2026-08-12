//! Runtime test composition helpers.

use std::sync::Arc;

use crate::agent_definition::AgentSpec;
use sylvander_llm_anthropic::{
    AnthropicProvider,
    api::{client::AnthropicClient, model::ModelCapabilities as AnthropicModelCapabilities},
};
use sylvander_llm_core::{
    ModelCapabilities as ProviderModelCapabilities, ModelInfo as ProviderModelInfo, ModelRef,
};

use crate::agent_run::{AgentRun, AgentRunBuilder};

fn provider_capabilities(capabilities: AnthropicModelCapabilities) -> ProviderModelCapabilities {
    let mut provider_capabilities = ProviderModelCapabilities::empty();
    for (anthropic, provider) in [
        (
            AnthropicModelCapabilities::EXTENDED_THINKING,
            ProviderModelCapabilities::REASONING,
        ),
        (
            AnthropicModelCapabilities::PROMPT_CACHING,
            ProviderModelCapabilities::PROMPT_CACHING,
        ),
        (
            AnthropicModelCapabilities::STRUCTURED_OUTPUT,
            ProviderModelCapabilities::STRUCTURED_OUTPUT,
        ),
        (
            AnthropicModelCapabilities::TOOL_USE,
            ProviderModelCapabilities::TOOL_USE,
        ),
        (
            AnthropicModelCapabilities::VISION,
            ProviderModelCapabilities::VISION,
        ),
        (
            AnthropicModelCapabilities::DOCUMENT_INPUT,
            ProviderModelCapabilities::DOCUMENT_INPUT,
        ),
    ] {
        if capabilities.contains(anthropic) {
            provider_capabilities |= provider;
        }
    }
    provider_capabilities
}

fn exact_anthropic_model(provider_id: &str, model: &ProviderModelInfo) -> ProviderModelInfo {
    ProviderModelInfo {
        reference: ModelRef::new(provider_id, &model.reference.model),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        capabilities: model.capabilities,
    }
}

pub(crate) fn qualified_anthropic_run_builder(
    spec: AgentSpec,
    client: AnthropicClient,
) -> AgentRunBuilder {
    let provider_id = spec.model.provider.clone();
    let model = spec.to_model_info().expect("valid test Agent model");
    let mut exact = exact_anthropic_model(&provider_id, &model);
    exact.capabilities = provider_capabilities(AnthropicModelCapabilities::empty());
    AgentRun::qualified_router_builder(
        spec,
        Arc::new(AnthropicProvider::new(&provider_id, client)),
        exact,
    )
}
