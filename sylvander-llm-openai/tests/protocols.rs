use futures_util::StreamExt as _;
use reqwest::Url;
use serde_json::json;
use sylvander_llm_core::{
    ChatMessage, ContentBlock, ImageContent, MediaSource, ModelProvider, ModelRef, ModelRequest,
    ModelStreamEvent, ReasoningConfig, ReasoningEffort, StopReason,
};
use sylvander_llm_openai::{
    OpenAiProtocol, OpenAiProvider, OpenAiProviderConfig, ProviderFeatures,
};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn request(provider: &str, model: &str) -> ModelRequest {
    ModelRequest {
        request_id: "request-1".into(),
        model: ModelRef::new(provider, model),
        system: Vec::new(),
        messages: vec![ChatMessage::user("hello")],
        tools: Vec::new(),
        max_output_tokens: 512,
        reasoning: None,
        output_schema: None,
    }
}

fn provider(
    server: &MockServer,
    provider_id: &str,
    protocol: OpenAiProtocol,
    features: ProviderFeatures,
) -> OpenAiProvider {
    OpenAiProvider::new(OpenAiProviderConfig {
        provider_id: provider_id.into(),
        base_url: Url::parse(&server.uri()).expect("mock URL"),
        api_key: "explicit-key".into(),
        protocol,
        features,
    })
    .expect("provider")
}

#[tokio::test]
async fn responses_matches_official_sdk_shape_for_gpt_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer explicit-key"))
        .and(body_partial_json(json!({
            "model": "gpt-5.6",
            "stream": true,
            "store": false,
            "max_output_tokens": 512,
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"type\":\"response.text.delta\",\"delta\":\"hi\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{",
                "\"id\":\"resp_1\",\"model\":\"gpt-5.6\",\"status\":\"completed\",",
                "\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}],",
                "\"usage\":{\"input_tokens\":7,\"output_tokens\":3,\"total_tokens\":10,",
                "\"input_tokens_details\":{\"cached_tokens\":2,\"cache_write_tokens\":1},",
                "\"output_tokens_details\":{\"reasoning_tokens\":1}}}}\n\n"
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let adapter = provider(
        &server,
        "openai",
        OpenAiProtocol::Responses,
        ProviderFeatures::default(),
    );
    let mut stream = adapter
        .complete_stream(request("openai", "gpt-5.6"))
        .await
        .expect("open stream");
    assert_eq!(
        stream.next().await.expect("delta").expect("valid delta"),
        ModelStreamEvent::TextDelta("hi".into())
    );
    let ModelStreamEvent::Completed(response) = stream
        .next()
        .await
        .expect("completed")
        .expect("valid response")
    else {
        panic!("expected completion");
    };
    assert_eq!(response.stop_reason, StopReason::EndTurn);
    assert_eq!(response.usage.cache_read_tokens, Some(2));
    assert_eq!(response.usage.details.reasoning_tokens, Some(1));
}

#[tokio::test]
async fn qwen_responses_extension_is_only_sent_when_enabled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(body_partial_json(json!({
            "model": "qwen3-max",
            "enable_thinking": true,
            "reasoning": {"effort": "high"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"type\":\"response.completed\",\"response\":{",
                "\"id\":\"resp_qwen\",\"model\":\"qwen3-max\",\"status\":\"completed\",",
                "\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,",
                "\"total_tokens\":2,\"input_tokens_details\":{\"cached_tokens\":0,\"cache_write_tokens\":0},",
                "\"output_tokens_details\":{\"reasoning_tokens\":1}}}}\n\n"
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let adapter = provider(
        &server,
        "qwen",
        OpenAiProtocol::Responses,
        ProviderFeatures::new(["enable_thinking"]),
    );
    let mut value = request("qwen", "qwen3-max");
    value.reasoning = Some(ReasoningConfig {
        budget_tokens: None,
        effort: Some(ReasoningEffort::High),
    });
    let mut stream = adapter.complete_stream(value).await.expect("open stream");
    assert!(matches!(
        stream.next().await.expect("event").expect("completion"),
        ModelStreamEvent::Completed(_)
    ));
}

#[tokio::test]
async fn chat_completions_assembles_deepseek_reasoning_and_tools() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({
            "model": "deepseek-reasoner",
            "stream": true,
            "max_tokens": 512
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"id\":\"chat_1\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"think\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chat_1\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"id\\\":1}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":4,\"total_tokens\":9,\"completion_tokens_details\":{\"reasoning_tokens\":2}}}\n\n",
                "data: [DONE]\n\n"
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let adapter = provider(
        &server,
        "deepseek",
        OpenAiProtocol::ChatCompletions,
        ProviderFeatures::new(["reasoning_content"]),
    );
    let mut stream = adapter
        .complete_stream(request("deepseek", "deepseek-reasoner"))
        .await
        .expect("open stream");
    assert_eq!(
        stream.next().await.expect("reasoning").expect("delta"),
        ModelStreamEvent::ReasoningDelta("think".into())
    );
    let ModelStreamEvent::Completed(response) =
        stream.next().await.expect("completion").expect("response")
    else {
        panic!("expected completion");
    };
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert!(matches!(
        response.content[0],
        ContentBlock::Reasoning { .. }
    ));
    assert!(matches!(response.content[1], ContentBlock::ToolCall { .. }));
    assert_eq!(response.usage.details.reported_total_tokens, Some(9));
}

#[tokio::test]
async fn qwen_chat_uses_configured_max_completion_tokens_feature() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({
            "model": "qwen-plus",
            "max_completion_tokens": 512
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"id\":\"chat_q\",\"model\":\"qwen-plus\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let adapter = provider(
        &server,
        "qwen",
        OpenAiProtocol::ChatCompletions,
        ProviderFeatures::new(["max_completion_tokens"]),
    );
    let mut stream = adapter
        .complete_stream(request("qwen", "qwen-plus"))
        .await
        .expect("open stream");
    assert!(matches!(
        stream.next().await.expect("delta").expect("event"),
        ModelStreamEvent::TextDelta(_)
    ));
    assert!(matches!(
        stream.next().await.expect("done").expect("completion"),
        ModelStreamEvent::Completed(_)
    ));
}

#[tokio::test]
async fn gpt_chat_keeps_text_and_image_in_one_official_user_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({
            "model": "gpt-4.1",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "describe"},
                    {"type": "image_url", "image_url": {"url": "https://example.test/image.png"}}
                ]
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"id\":\"chat_image\",\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let mut value = request("openai", "gpt-4.1");
    value.messages = vec![ChatMessage::user_blocks(vec![
        ContentBlock::Text {
            text: "describe".into(),
        },
        ContentBlock::Image {
            image: ImageContent {
                source: MediaSource::Url {
                    url: "https://example.test/image.png".into(),
                },
                alt_text: None,
            },
        },
    ])];
    let mut stream = provider(
        &server,
        "openai",
        OpenAiProtocol::ChatCompletions,
        ProviderFeatures::default(),
    )
    .complete_stream(value)
    .await
    .expect("open stream");
    assert!(matches!(
        stream.next().await.expect("delta").expect("event"),
        ModelStreamEvent::TextDelta(_)
    ));
    assert!(matches!(
        stream.next().await.expect("done").expect("completion"),
        ModelStreamEvent::Completed(_)
    ));
}
