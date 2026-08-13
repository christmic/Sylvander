use std::time::Duration;

use sylvander_benchmark_agent::provider::{
    AgentProtocol, AgentProviderBinding, ProviderBuildError, build_provider,
};

fn binding(protocol: AgentProtocol) -> AgentProviderBinding {
    AgentProviderBinding {
        provider_id: "provider".into(),
        protocol,
        base_url: "https://example.com/v1".into(),
        provider_features: Vec::new(),
    }
}

#[test]
fn constructs_every_supported_text_protocol_adapter() {
    for protocol in [
        AgentProtocol::AnthropicMessages,
        AgentProtocol::OpenAiResponses,
        AgentProtocol::OpenAiChatCompletions,
        AgentProtocol::DashScopeGeneration,
    ] {
        build_provider(&binding(protocol), "secret".into(), Duration::from_secs(1)).unwrap();
    }
}

#[test]
fn parses_stable_protocol_coordinates_and_rejects_unknown_values() {
    assert_eq!(
        AgentProtocol::parse("anthropic_compatible"),
        Ok(AgentProtocol::AnthropicMessages)
    );
    assert_eq!(
        AgentProtocol::parse("openai_responses"),
        Ok(AgentProtocol::OpenAiResponses)
    );
    assert_eq!(
        AgentProtocol::parse("unknown"),
        Err(ProviderBuildError::UnsupportedProtocol)
    );
}

#[test]
fn provider_features_are_validated_by_the_selected_protocol() {
    let mut anthropic = binding(AgentProtocol::AnthropicMessages);
    anthropic.provider_features.push("reasoning_content".into());
    assert!(matches!(
        build_provider(&anthropic, "secret".into(), Duration::from_secs(1)),
        Err(ProviderBuildError::InvalidConfiguration)
    ));

    let mut responses = binding(AgentProtocol::OpenAiResponses);
    responses.provider_features.push("reasoning_content".into());
    assert!(matches!(
        build_provider(&responses, "secret".into(), Duration::from_secs(1)),
        Err(ProviderBuildError::InvalidConfiguration)
    ));
}
