//! `OpenAI` HTTP error metadata preservation.
//!
//! Evidence: openai-python `a1eeab58`, `_base_client.py` and error response models.

use reqwest::Url;
use sylvander_llm_core::{ModelProvider, ModelRef, ModelRequest, ProviderErrorKind};
use sylvander_llm_openai::{
    OpenAiProtocol, OpenAiProvider, OpenAiProviderConfig, ProviderFeatures,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn protocol_rejects_unknown_feature_at_construction() {
    let error = OpenAiProvider::new(OpenAiProviderConfig {
        provider_id: "openai".into(),
        base_url: Url::parse("https://api.openai.com").expect("URL"),
        api_key: "key".into(),
        protocol: OpenAiProtocol::Responses,
        features: ProviderFeatures::new(["chat_only_extension"]),
    })
    .expect_err("unknown feature");
    assert_eq!(error.kind, ProviderErrorKind::InvalidRequest);
}

#[tokio::test]
async fn rate_limit_preserves_request_id_and_retry_delay() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("x-request-id", "req_rate")
                .insert_header("retry-after-ms", "250")
                .set_body_json(serde_json::json!({
                    "error": {"type": "rate_limit_error", "message": "limited"}
                })),
        )
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(OpenAiProviderConfig {
        provider_id: "openai".into(),
        base_url: Url::parse(&server.uri()).expect("mock URL"),
        api_key: "key".into(),
        protocol: OpenAiProtocol::Responses,
        features: ProviderFeatures::default(),
    })
    .expect("provider");
    let error = provider
        .complete_stream(ModelRequest {
            request_id: "request".into(),
            model: ModelRef::new("openai", "gpt-5.6"),
            system: Vec::new(),
            messages: vec![sylvander_llm_core::ChatMessage::user("hello")],
            tools: Vec::new(),
            max_output_tokens: 32,
            reasoning: None,
            output_schema: None,
        })
        .await
        .err()
        .expect("rate limit");
    assert_eq!(error.kind, ProviderErrorKind::RateLimited);
    assert_eq!(error.status, Some(429));
    assert_eq!(error.request_id.as_deref(), Some("req_rate"));
    assert_eq!(error.retry_after_ms, Some(250));
}
