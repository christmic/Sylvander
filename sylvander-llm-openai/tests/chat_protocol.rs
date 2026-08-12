//! Official-derived Chat Completions stream termination and usage contracts.
//!
//! Evidence: openai-python `a1eeab58`, `chat_completion_chunk.py`.

use futures_util::StreamExt as _;
use reqwest::Url;
use sylvander_llm_core::{ModelProvider, ModelRef, ModelRequest, ProviderErrorKind};
use sylvander_llm_openai::{
    OpenAiProtocol, OpenAiProvider, OpenAiProviderConfig, ProviderFeatures,
};
use wiremock::matchers::body_string_contains;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn request() -> ModelRequest {
    ModelRequest {
        request_id: "request-chat".into(),
        model: ModelRef::new("openai", "gpt-4.1"),
        system: Vec::new(),
        messages: vec![sylvander_llm_core::ChatMessage::user("hello")],
        tools: Vec::new(),
        max_output_tokens: 64,
        reasoning: None,
        output_schema: None,
    }
}

fn provider(server: &MockServer) -> OpenAiProvider {
    OpenAiProvider::new(OpenAiProviderConfig {
        provider_id: "openai".into(),
        base_url: Url::parse(&server.uri()).expect("mock URL"),
        api_key: "key".into(),
        protocol: OpenAiProtocol::ChatCompletions,
        features: ProviderFeatures::new(["max_completion_tokens"]),
    })
    .expect("provider")
}

#[tokio::test]
async fn requests_the_official_usage_tail_chunk() {
    let server = MockServer::start().await;
    Mock::given(body_string_contains(
        "\"stream_options\":{\"include_usage\":true}",
    ))
    .respond_with(ResponseTemplate::new(200).set_body_raw(
        concat!(
            "data: {\"id\":\"chat_1\",\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}],\"usage\":null}\n\n",
            "data: {\"id\":\"chat_1\",\"model\":\"gpt-4.1\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        ),
        "text/event-stream",
    ))
    .mount(&server)
    .await;
    let mut stream = provider(&server)
        .complete_stream(request())
        .await
        .expect("open stream");
    while stream.next().await.is_some() {}
    server.verify().await;
}

#[tokio::test]
async fn done_without_usage_fails_instead_of_fabricating_zero() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"id\":\"chat_1\",\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}],\"usage\":null}\n\n",
                "data: [DONE]\n\n"
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let mut stream = provider(&server)
        .complete_stream(request())
        .await
        .expect("open stream");
    let _ = stream.next().await.expect("text").expect("delta");
    let error = stream
        .next()
        .await
        .expect("terminal")
        .expect_err("usage error");
    assert_eq!(error.kind, ProviderErrorKind::Protocol);
}

#[tokio::test]
async fn eof_without_done_is_a_protocol_error() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"id\":\"chat_1\",\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}],\"usage\":null}\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let mut stream = provider(&server)
        .complete_stream(request())
        .await
        .expect("open stream");
    let _ = stream.next().await.expect("text").expect("delta");
    let error = stream
        .next()
        .await
        .expect("error")
        .expect_err("protocol error");
    assert_eq!(error.kind, ProviderErrorKind::Protocol);
}
