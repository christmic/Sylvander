//! Official-derived native Generation SSE error and truncation contracts.
//!
//! Evidence: dashscope-sdk-python `397e02b`, `common/utils.py` and
//! `api_entities/http_request.py`.

use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::Url;
use sylvander_llm_core::{
    ModelProvider, ModelRef, ModelRequest, ProviderErrorKind, ProviderErrorPhase,
};
use sylvander_llm_dashscope::{
    DashScopeFeatures, DashScopeProtocol, DashScopeProvider, DashScopeProviderConfig,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn generation_rejects_unknown_feature_at_construction() {
    let error = DashScopeProvider::new(DashScopeProviderConfig {
        provider_id: "dashscope".into(),
        base_url: Url::parse("https://dashscope.aliyuncs.com").expect("URL"),
        api_key: "key".into(),
        protocol: DashScopeProtocol::TextGeneration,
        features: DashScopeFeatures::new(["response_format"]),
    })
    .expect_err("unknown feature");
    assert_eq!(error.kind, ProviderErrorKind::InvalidRequest);
}

fn request() -> ModelRequest {
    ModelRequest {
        request_id: "request".into(),
        model: ModelRef::new("dashscope", "qwen-plus"),
        system: Vec::new(),
        messages: vec![sylvander_llm_core::ChatMessage::user("hello")],
        tools: Vec::new(),
        max_output_tokens: 64,
        reasoning: None,
        output_schema: None,
    }
}

fn provider(server: &MockServer) -> DashScopeProvider {
    DashScopeProvider::new(DashScopeProviderConfig {
        provider_id: "dashscope".into(),
        base_url: Url::parse(&server.uri()).expect("mock URL"),
        api_key: "key".into(),
        protocol: DashScopeProtocol::TextGeneration,
        features: DashScopeFeatures::default(),
    })
    .expect("provider")
}

#[tokio::test]
async fn sse_error_preserves_status_and_request_id() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "event:error\n",
                "status:429\n",
                "data:{\"request_id\":\"req_limit\",\"code\":\"Throttling\",\"message\":\"limited\"}\n\n"
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let mut stream = provider(&server)
        .complete_stream(request())
        .await
        .expect("open stream");
    let error = stream.next().await.expect("error").expect_err("API error");
    assert_eq!(error.kind, ProviderErrorKind::RateLimited);
    assert_eq!(error.status, Some(429));
    assert_eq!(error.request_id.as_deref(), Some("req_limit"));
}

#[tokio::test]
async fn payment_required_is_non_retryable_quota_exhaustion() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(402).set_body_json(serde_json::json!({
            "code": "Arrearage", "message": "empty"
        })))
        .mount(&server)
        .await;
    let error = provider(&server)
        .complete_stream(request())
        .await
        .err()
        .expect("quota error");
    assert_eq!(error.kind, ProviderErrorKind::QuotaExceeded);
    assert_eq!(error.status, Some(402));
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn eof_without_finish_reason_fails_closed() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"request_id\":\"req_partial\",\"output\":{\"choices\":[{\"message\":{\"content\":\"partial\"},\"finish_reason\":null}]},\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n\n",
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

#[tokio::test]
async fn output_schema_is_rejected_instead_of_downgraded_to_json_object() {
    let server = MockServer::start().await;
    let mut value = request();
    value.output_schema = Some(serde_json::json!({
        "type": "object",
        "required": ["answer"]
    }));
    let error = provider(&server)
        .complete_stream(value)
        .await
        .err()
        .expect("unsupported schema");
    assert_eq!(error.kind, ProviderErrorKind::Unsupported);
}

#[tokio::test]
async fn configured_deadline_is_a_retryable_open_timeout() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(200)))
        .mount(&server)
        .await;
    let provider = DashScopeProvider::new_with_timeout(
        DashScopeProviderConfig {
            provider_id: "dashscope".into(),
            base_url: Url::parse(&server.uri()).expect("mock URL"),
            api_key: "key".into(),
            protocol: DashScopeProtocol::TextGeneration,
            features: DashScopeFeatures::default(),
        },
        Duration::from_millis(20),
    )
    .expect("provider");
    let error = provider
        .complete_stream(request())
        .await
        .err()
        .expect("deadline");
    assert_eq!(error.kind, ProviderErrorKind::Timeout);
    assert_eq!(error.phase, ProviderErrorPhase::Open);
    assert!(error.is_retryable());
}
