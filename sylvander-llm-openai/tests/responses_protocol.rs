//! Official-derived `OpenAI` Responses terminal and replay contracts.
//!
//! Evidence: openai-python `a1eeab58`, `src/openai/types/responses/`.

use futures_util::StreamExt as _;
use reqwest::Url;
use serde_json::json;
use sylvander_llm_core::{
    ChatMessage, ContentBlock, DocumentContent, MediaSource, ModelProvider, ModelRef, ModelRequest,
    ModelStreamEvent, OpaqueProviderState, ReasoningConfig, ReasoningEffort, StopReason,
    ToolDefinition,
};
use sylvander_llm_openai::{
    OpenAiProtocol, OpenAiProvider, OpenAiProviderConfig, ProviderFeatures,
};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn request() -> ModelRequest {
    ModelRequest {
        request_id: "request-responses".into(),
        model: ModelRef::new("openai", "gpt-5.6"),
        system: Vec::new(),
        messages: vec![ChatMessage::user("hello")],
        tools: Vec::new(),
        max_output_tokens: 128,
        reasoning: None,
        output_schema: None,
    }
}

fn provider(server: &MockServer) -> OpenAiProvider {
    OpenAiProvider::new(OpenAiProviderConfig {
        provider_id: "openai".into(),
        base_url: Url::parse(&server.uri()).expect("mock URL"),
        api_key: "key".into(),
        protocol: OpenAiProtocol::Responses,
        features: ProviderFeatures::default(),
    })
    .expect("provider")
}

#[tokio::test]
async fn incomplete_is_a_terminal_response_with_max_output_reason() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"type\":\"response.incomplete\",\"sequence_number\":3,\"response\":{",
                "\"id\":\"resp_incomplete\",\"model\":\"gpt-5.6\",\"status\":\"incomplete\",",
                "\"output\":[],\"incomplete_details\":{\"reason\":\"max_output_tokens\"},",
                "\"usage\":{\"input_tokens\":2,\"output_tokens\":128,\"total_tokens\":130,",
                "\"input_tokens_details\":{\"cached_tokens\":0},",
                "\"output_tokens_details\":{\"reasoning_tokens\":64}}}}\n\n"
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let mut stream = provider(&server)
        .complete_stream(request())
        .await
        .expect("open stream");
    let ModelStreamEvent::Completed(response) =
        stream.next().await.expect("terminal").expect("valid event")
    else {
        panic!("expected completion");
    };
    assert_eq!(response.stop_reason, StopReason::MaxOutputTokens);
    assert_eq!(response.usage.output_tokens, 128);
    assert_eq!(response.usage.cache_write_tokens, Some(0));
}

#[tokio::test]
async fn reasoning_state_is_requested_and_replayed_losslessly() {
    let server = MockServer::start().await;
    let reasoning = json!({
        "type": "reasoning",
        "id": "rs_1",
        "summary": [{"type": "summary_text", "text": "plan"}],
        "encrypted_content": "opaque",
        "status": "completed"
    });
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(body_partial_json(json!({
            "include": ["reasoning.encrypted_content"],
            "input": [reasoning.clone(), {
                "role": "user",
                "content": [{"type": "input_text", "text": "continue"}]
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"type\":\"response.completed\",\"sequence_number\":5,\"response\":{",
                "\"id\":\"resp_2\",\"model\":\"gpt-5.6\",\"status\":\"completed\",",
                "\"output\":[{\"type\":\"reasoning\",\"id\":\"rs_2\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"next\"}],\"encrypted_content\":\"opaque-2\",\"status\":\"completed\"}],",
                "\"usage\":{\"input_tokens\":4,\"output_tokens\":2,\"total_tokens\":6,",
                "\"input_tokens_details\":{\"cached_tokens\":0,\"cache_write_tokens\":0},",
                "\"output_tokens_details\":{\"reasoning_tokens\":2}}}}\n\n"
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let mut value = request();
    value.reasoning = Some(ReasoningConfig {
        budget_tokens: None,
        effort: Some(ReasoningEffort::High),
    });
    value.messages = vec![
        ChatMessage::assistant(vec![ContentBlock::Reasoning {
            text: "plan".into(),
            opaque_state: Some(OpaqueProviderState {
                provider: "openai".into(),
                data: reasoning,
            }),
        }]),
        ChatMessage::user("continue"),
    ];
    let mut stream = provider(&server)
        .complete_stream(value)
        .await
        .expect("open stream");
    let ModelStreamEvent::Completed(response) =
        stream.next().await.expect("terminal").expect("response")
    else {
        panic!("expected completion");
    };
    assert!(matches!(
        response.content.first(),
        Some(ContentBlock::Reasoning {
            opaque_state: Some(_),
            ..
        })
    ));
}

#[tokio::test]
async fn unknown_response_event_fails_closed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"type\":\"response.future_event\"}\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let mut stream = provider(&server)
        .complete_stream(request())
        .await
        .expect("open stream");
    let error = stream
        .next()
        .await
        .expect("error event")
        .expect_err("error");
    assert_eq!(error.kind, sylvander_llm_core::ProviderErrorKind::Protocol);
}

#[tokio::test]
async fn documents_tools_and_strict_schema_match_official_request_types() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(body_partial_json(json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_file",
                    "file_url": "https://example.test/spec.pdf",
                    "filename": "spec.pdf"
                }]
            }],
            "tools": [{
                "type": "function",
                "name": "lookup",
                "description": "Look up an item",
                "parameters": {"type": "object"},
                "strict": false
            }],
            "text": {"format": {
                "type": "json_schema",
                "name": "response",
                "schema": {"type": "object", "required": ["answer"]},
                "strict": true
            }}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"type\":\"response.completed\",\"response\":{",
                "\"id\":\"resp_typed\",\"model\":\"gpt-5.6\",\"status\":\"completed\",",
                "\"output\":[],\"usage\":{\"input_tokens\":8,\"output_tokens\":1,\"total_tokens\":9,",
                "\"input_tokens_details\":{\"cached_tokens\":0,\"cache_write_tokens\":0},",
                "\"output_tokens_details\":{\"reasoning_tokens\":0}}}}\n\n"
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let mut value = request();
    value.messages = vec![ChatMessage::user_blocks(vec![ContentBlock::Document {
        document: DocumentContent {
            source: MediaSource::Url {
                url: "https://example.test/spec.pdf".into(),
            },
            title: Some("spec.pdf".into()),
        },
    }])];
    value.tools = vec![ToolDefinition {
        name: "lookup".into(),
        description: "Look up an item".into(),
        input_schema: json!({"type": "object"}),
        cache_hint: None,
    }];
    value.output_schema = Some(json!({"type": "object", "required": ["answer"]}));
    let mut stream = provider(&server)
        .complete_stream(value)
        .await
        .expect("open stream");
    assert!(matches!(
        stream.next().await.expect("terminal").expect("response"),
        ModelStreamEvent::Completed(_)
    ));
}
