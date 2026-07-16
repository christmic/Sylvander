use futures_util::StreamExt;
use serde_json::json;
use sylvander_llm_core::{
    ChatMessage, ContentBlock, ModelProvider, ModelRef, ModelRequest, ModelStreamEvent,
    ProviderErrorKind,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

fn provider(server: &MockServer) -> AnthropicProvider {
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .build()
        .unwrap();
    AnthropicProvider::new("anthropic", client)
}

fn request() -> ModelRequest {
    ModelRequest {
        request_id: "req-1".into(),
        model: ModelRef::new("anthropic", "claude-test"),
        system: Vec::new(),
        messages: vec![ChatMessage::user("hello")],
        tools: Vec::new(),
        max_output_tokens: 100,
        reasoning: None,
        output_schema: None,
    }
}

const TEXT_STREAM: &str = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-test\",\"stop_reason\":null,\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":0}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

#[tokio::test]
async fn text_stream_completes_once_and_last_with_one_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(TEXT_STREAM, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let events = provider(&server)
        .complete_stream(request())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(&events[0], Ok(ModelStreamEvent::TextDelta(text)) if text == "hello"));
    assert!(matches!(
        events.last(),
        Some(Ok(ModelStreamEvent::Completed(_)))
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Ok(ModelStreamEvent::Completed(_))))
            .count(),
        1
    );
}

#[tokio::test]
async fn reasoning_and_tool_call_are_preserved_in_completion() {
    let server = MockServer::start().await;
    let body = TEXT_STREAM
        .replace(
            "{\"type\":\"text\",\"text\":\"\"}",
            "{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}",
        )
        .replace(
            "{\"type\":\"text_delta\",\"text\":\"hello\"}",
            "{\"type\":\"thinking_delta\",\"thinking\":\"think\"}",
        )
        .replace(
            "event: content_block_stop",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"signed\"}}\n\nevent: content_block_stop",
        )
        .replace("\"stop_reason\":\"end_turn\"", "\"stop_reason\":\"tool_use\"")
        .replace(
            "event: message_delta",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call-1\",\"name\":\"read\",\"input\":{}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"/tmp/a\\\"}\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\nevent: message_delta",
        );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;
    let events = provider(&server)
        .complete_stream(request())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(&events[0], Ok(ModelStreamEvent::ReasoningDelta(text)) if text == "think"));
    let Ok(ModelStreamEvent::Completed(response)) = events.last().unwrap() else {
        panic!("expected completion");
    };
    assert!(
        matches!(&response.content[0], ContentBlock::Reasoning { opaque_state: Some(state), .. } if state.data["signature"] == "signed")
    );
    assert!(matches!(&response.content[1], ContentBlock::ToolCall { name, .. } if name == "read"));
}

#[tokio::test]
async fn open_error_is_redacted_and_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "type": "error",
            "error": {"type": "rate_limit_error", "message": "secret-marker"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let Err(error) = provider(&server).complete_stream(request()).await else {
        panic!("expected open error");
    };
    assert_eq!(error.kind, ProviderErrorKind::RateLimited);
    assert!(!format!("{error:?}").contains("secret-marker"));
}

#[tokio::test]
async fn malformed_mid_stream_terminates_without_completion() {
    let server = MockServer::start().await;
    let body = format!("{TEXT_STREAM}event: content_block_delta\ndata: not-json\n\n");
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;
    let events = provider(&server)
        .complete_stream(request())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Err(error) if error.kind == ProviderErrorKind::Protocol))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(ModelStreamEvent::Completed(_))))
    );
}
