use futures_util::StreamExt as _;
use reqwest::Url;
use serde_json::json;
use sylvander_llm_core::{
    ChatMessage, ContentBlock, ImageContent, MediaSource, ModelProvider, ModelRef, ModelRequest,
    ModelStreamEvent, ReasoningConfig, StopReason,
};
use sylvander_llm_dashscope::{
    DashScopeFeatures, DashScopeProtocol, DashScopeProvider, DashScopeProviderConfig,
};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn request(model: &str) -> ModelRequest {
    ModelRequest {
        request_id: "request-1".into(),
        model: ModelRef::new("dashscope", model),
        system: Vec::new(),
        messages: vec![ChatMessage::user("hello")],
        tools: Vec::new(),
        max_output_tokens: 256,
        reasoning: None,
        output_schema: None,
    }
}

#[tokio::test]
async fn multimodal_generation_uses_the_native_image_endpoint_and_shape() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/services/aigc/multimodal-generation/generation",
        ))
        .and(body_partial_json(json!({
            "model": "qwen3.7-plus",
            "input": {"messages": [{
                "role": "user",
                "content": [
                    {"image": "data:image/png;base64,cG5n"},
                    {"text": "identify"}
                ]
            }]}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"request_id\":\"req_image\",\"output\":{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"7\"},\"finish_reason\":\"stop\"}]},",
                "\"usage\":{\"input_tokens\":12,\"output_tokens\":1,\"total_tokens\":13}}\n\n"
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let provider = DashScopeProvider::new(DashScopeProviderConfig {
        provider_id: "dashscope".into(),
        base_url: Url::parse(&server.uri()).expect("mock URL"),
        api_key: "key".into(),
        protocol: DashScopeProtocol::MultimodalGeneration,
        features: DashScopeFeatures::default(),
    })
    .expect("provider");
    let mut value = request("qwen3.7-plus");
    value.messages = vec![ChatMessage::user_blocks(vec![
        ContentBlock::Image {
            image: ImageContent {
                source: MediaSource::Base64 {
                    media_type: "image/png".into(),
                    data: "cG5n".into(),
                },
                alt_text: None,
            },
        },
        ContentBlock::Text {
            text: "identify".into(),
        },
    ])];
    let mut stream = provider.complete_stream(value).await.expect("open stream");
    assert!(matches!(
        stream.next().await.expect("text").expect("event"),
        ModelStreamEvent::TextDelta(ref value) if value == "7"
    ));
}

fn provider(server: &MockServer, features: DashScopeFeatures) -> DashScopeProvider {
    DashScopeProvider::new(DashScopeProviderConfig {
        provider_id: "dashscope".into(),
        base_url: Url::parse(&server.uri()).expect("mock URL"),
        api_key: "explicit-key".into(),
        protocol: DashScopeProtocol::TextGeneration,
        features,
    })
    .expect("provider")
}

#[tokio::test]
async fn qwen_plus_matches_official_generation_request_and_response_shape() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/services/aigc/text-generation/generation"))
        .and(header("authorization", "Bearer explicit-key"))
        .and(header("x-dashscope-sse", "enable"))
        .and(body_partial_json(json!({
            "model": "qwen-plus",
            "input": {"messages": [{"role": "user", "content": "hello"}]},
            "parameters": {
                "result_format": "message",
                "incremental_output": true,
                "max_tokens": 256
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"request_id\":\"req_qwen\",\"output\":{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":\"stop\"}]},",
                "\"usage\":{\"input_tokens\":2,\"output_tokens\":1,\"total_tokens\":3,",
                "\"prompt_tokens_details\":{\"cached_tokens\":1},",
                "\"output_tokens_details\":{\"reasoning_tokens\":0}}}\n\n"
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let mut stream = provider(&server, DashScopeFeatures::default())
        .complete_stream(request("qwen-plus"))
        .await
        .expect("open stream");
    assert_eq!(
        stream.next().await.expect("delta").expect("event"),
        ModelStreamEvent::TextDelta("hello".into())
    );
    let ModelStreamEvent::Completed(response) =
        stream.next().await.expect("completion").expect("event")
    else {
        panic!("expected completion");
    };
    assert_eq!(response.stop_reason, StopReason::EndTurn);
    assert_eq!(response.usage.details.reported_total_tokens, Some(3));
    assert_eq!(response.usage.cache_read_tokens, Some(1));
    assert_eq!(response.usage.details.reasoning_tokens, Some(0));
}

#[tokio::test]
async fn qwen3_max_thinking_budget_requires_explicit_features() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/services/aigc/text-generation/generation"))
        .and(body_partial_json(json!({
            "model": "qwen3-max",
            "parameters": {
                "enable_thinking": true,
                "thinking_budget": 128
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"request_id\":\"req_think\",\"output\":{\"choices\":[{\"message\":{\"role\":\"assistant\",\"reasoning_content\":\"plan\",\"content\":\"\"},\"finish_reason\":null}]},\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}\n\n",
                "data: {\"request_id\":\"req_think\",\"output\":{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"done\"},\"finish_reason\":\"stop\"}]},\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}\n\n"
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let mut value = request("qwen3-max");
    value.reasoning = Some(ReasoningConfig {
        budget_tokens: Some(128),
        effort: None,
    });
    let mut stream = provider(
        &server,
        DashScopeFeatures::new(["enable_thinking", "thinking_budget"]),
    )
    .complete_stream(value)
    .await
    .expect("open stream");
    assert!(matches!(
        stream.next().await.expect("reasoning").expect("event"),
        ModelStreamEvent::ReasoningDelta(_)
    ));
    assert!(matches!(
        stream.next().await.expect("text").expect("event"),
        ModelStreamEvent::TextDelta(_)
    ));
    let ModelStreamEvent::Completed(response) =
        stream.next().await.expect("completion").expect("event")
    else {
        panic!("expected completion");
    };
    assert!(matches!(
        response.content[0],
        ContentBlock::Reasoning { .. }
    ));
    assert!(matches!(response.content[1], ContentBlock::Text { .. }));
}
