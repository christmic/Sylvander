//! Integration tests for `POST /v1/messages` (streaming).

use futures_util::StreamExt;
use serde_json::json;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::error::AnthropicError;
// ModelId removed; pass model string directly
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::{ContentDelta, ContentBlock, MessageParam, RawStreamEvent, StopReason};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mock_client(server: &MockServer) -> AnthropicClient {
    AnthropicClient::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .build()
        .expect("build should succeed")
}

fn minimal_request() -> CreateMessageRequest {
    CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Tell me a story")])
        .build()
        .expect("build should succeed")
}

const SAMPLE_STREAM: &str = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_stream1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-5-20260601\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Once \"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"upon a \"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"time...\"}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":0}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":10}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

#[tokio::test]
async fn stream_full_assembly() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("accept", "*/*"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(SAMPLE_STREAM, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let mut stream = client
        .messages()
        .stream(&minimal_request())
        .await
        .expect("stream should succeed");

    let mut event_count = 0;
    while let Some(event) = stream.next().await {
        let event = event.expect("event should be Ok");
        event_count += 1;
        if let RawStreamEvent::ContentBlockDelta { delta, .. } = event
            && let ContentDelta::TextDelta { text } = delta
        {
            assert!(text == "Once " || text == "upon a " || text == "time...");
        }
    }
    assert_eq!(event_count, 8);

    let final_msg = stream
        .final_message()
        .expect("final_message should be available after MessageStop");
    assert_eq!(final_msg.id, "msg_stream1");
    assert_eq!(final_msg.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(final_msg.content.len(), 1);
    match &final_msg.content[0] {
        ContentBlock::Text(t) => assert_eq!(t.text, "Once upon a time..."),
        other => panic!("expected Text block, got {other:?}"),
    }
    assert_eq!(final_msg.usage.output_tokens, 10);
}

#[tokio::test]
async fn stream_with_tool_use_assembles_input_json() {
    let server = MockServer::start().await;

    let stream_body = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_t\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-5-20260601\",\"stop_reason\":null,\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_xyz\",\"name\":\"Read\",\"input\":{}}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"fil\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"e_path\\\": \\\"/a.tx\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"t\\\"}\"}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":0}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":15}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(stream_body, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let mut stream = client
        .messages()
        .stream(&minimal_request())
        .await
        .expect("stream should succeed");

    while let Some(event) = stream.next().await {
        event.expect("event should be Ok");
    }

    let final_msg = stream.final_message().expect("final_message");
    assert_eq!(final_msg.stop_reason, Some(StopReason::ToolUse));
    let tool_use = final_msg.first_tool_use().expect("tool_use block");
    assert_eq!(tool_use.id, "toolu_xyz");
    assert_eq!(tool_use.name, "Read");
    assert_eq!(tool_use.input["file_path"], "/a.txt");
}

#[tokio::test]
async fn stream_400_api_error_returns_typed_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "invalid_request_error",
            "message": "stream requires stream: true",
            "request_id": "req_stream_err"
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let result = client.messages().stream(&minimal_request()).await;
    match result {
        Err(AnthropicError::Api {
            status,
            error_type,
            request_id,
            ..
        }) => {
            assert_eq!(status, 400);
            assert_eq!(error_type, "invalid_request_error");
            assert_eq!(request_id.as_deref(), Some("req_stream_err"));
        }
        Ok(_) => panic!("expected Api error, got Ok"),
        Err(other) => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn stream_wrong_content_type_errors() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"error": "nope"})),
        )
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let result = client.messages().stream(&minimal_request()).await;
    match result {
        Err(AnthropicError::Validation(msg)) => {
            assert!(msg.contains("text/event-stream"));
        }
        Ok(_) => panic!("expected Validation error, got Ok"),
        Err(other) => panic!("expected Validation error, got {other:?}"),
    }
}
#[tokio::test]
async fn stream_citations_delta_strongly_typed() {
    let server = MockServer::start().await;

    let stream_body = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_cite\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-5-20260601\",\"stop_reason\":null,\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"citations_delta\",\"citation\":{\"type\":\"char_location\",\"cited_text\":\"hello world\",\"document_index\":0,\"start_char_index\":6,\"end_char_index\":11}}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":0}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(stream_body, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let mut stream = client
        .messages()
        .stream(&minimal_request())
        .await
        .expect("stream should succeed");

    let mut found_citation = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("event should be Ok");
        if let sylvander_llm_anthropic::api::types::RawStreamEvent::ContentBlockDelta {
            delta:
                sylvander_llm_anthropic::api::types::ContentDelta::CitationsDelta {
                    citation:
                        sylvander_llm_anthropic::api::types::TextCitation::CharLocation(c),
                },
            ..
        } = &event
        {
            assert_eq!(c.cited_text, "hello world");
            assert_eq!(c.document_index, 0);
            assert_eq!(c.start_char_index, 6);
            assert_eq!(c.end_char_index, 11);
            found_citation = true;
        }
    }
    assert!(found_citation, "expected to find citations_delta event");
}
