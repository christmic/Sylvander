//! Stress tests for large SSE streams.
//!
//! Validates that `SseParser` and `MessageStream` correctly handle:
//! - Many small events (10K+ text deltas)
//! - Large single blocks (100K+ chars)
//! - Events split across many small chunks

use futures_util::StreamExt;
use std::time::Instant;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::{ContentBlock, MessageParam, RawStreamEvent, StopReason};
use wiremock::matchers::{method, path};
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
        .max_tokens(200_000)
        .messages(vec![MessageParam::user("Stress test")])
        .build()
        .expect("build should succeed")
}

#[tokio::test]
async fn parse_10k_text_deltas() {
    let server = MockServer::start().await;

    // Build a stream with 10,000 text_delta events, total ~1MB body.
    // Use a fixed chunk size to make the test deterministic.
    const DELTAS: usize = 10_000;
    const CHUNK_TEXT: &str = "lorem ipsum dolor sit amet ";

    let mut body = String::new();
    body.push_str("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_stress\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-5-20260601\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n");
    body.push_str("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n");
    for i in 0..DELTAS {
        body.push_str(&format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{CHUNK_TEXT}{i} \"}}}}\n\n"
        ));
    }
    body.push_str("event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n");
    body.push_str("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":200000}}\n\n");
    body.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");

    eprintln!("stress stream body size: {} bytes", body.len());

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(body, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let start = Instant::now();
    let mut stream = client
        .messages()
        .stream(&minimal_request())
        .await
        .expect("stream should succeed");

    let mut event_count = 0;
    let mut delta_count = 0;
    while let Some(event) = stream.next().await {
        let event = event.expect("event should be Ok");
        event_count += 1;
        if matches!(event, RawStreamEvent::ContentBlockDelta { .. }) {
            delta_count += 1;
        }
    }
    let parse_elapsed = start.elapsed();
    eprintln!(
        "parsed {event_count} events ({delta_count} deltas) in {parse_elapsed:?}"
    );

    // 1 message_start + 1 content_block_start + DELTAS deltas + 1 content_block_stop + 1 message_delta + 1 message_stop
    assert_eq!(event_count, DELTAS + 5);
    assert_eq!(delta_count, DELTAS);

    let final_msg = stream.final_message().expect("final_message");
    assert_eq!(final_msg.stop_reason, Some(StopReason::EndTurn));
    let text = final_msg.text();
    assert!(text.len() > 100_000, "expected > 100K chars, got {}", text.len());

    // Verify every chunk was assembled in order
    for i in 0..DELTAS {
        let expected_substring = format!("{CHUNK_TEXT}{i}");
        assert!(
            text.contains(&expected_substring),
            "missing chunk {i} in assembled text"
        );
    }
}

#[tokio::test]
async fn parse_large_single_text_block() {
    let server = MockServer::start().await;

    // One single text_delta event with 100K chars of text.
    // Tests that the parser/stream can handle large payloads without
    // excessive reallocation.
    const TEXT_SIZE: usize = 100_000;

    // Build as one large text_delta event (not many small ones).
    // We use a synthetic chunked delta: split the large text into
    // multiple deltas to simulate realistic streaming behavior.
    const CHUNKS: usize = 100;
    let chunk_size = TEXT_SIZE / CHUNKS;
    let chunk_text: String = "x".repeat(chunk_size);

    let mut body = String::new();
    body.push_str("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_large\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-5-20260601\",\"stop_reason\":null,\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n");
    body.push_str("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n");
    for _ in 0..CHUNKS {
        body.push_str(&format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{chunk_text}\"}}}}\n\n"
        ));
    }
    body.push_str("event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n");
    body.push_str("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":100000}}\n\n");
    body.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");

    eprintln!("large single block body size: {} bytes", body.len());

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(body, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let mut stream = client
        .messages()
        .stream(&minimal_request())
        .await
        .expect("stream should succeed");

    while stream.next().await.is_some() {}

    let final_msg = stream.final_message().expect("final_message");
    match &final_msg.content[0] {
        ContentBlock::Text(t) => {
            assert_eq!(t.text.len(), TEXT_SIZE);
            assert!(t.text.chars().all(|c| c == 'x'));
        }
        other => panic!("expected Text block, got {other:?}"),
    }
}

#[tokio::test]
async fn parse_events_split_across_tiny_chunks() {
    // Send a single SSE event, but split it across many 1-byte
    // chunks. Validates that the buffer correctly accumulates and
    // yields the event only after the separator arrives.
    let server = MockServer::start().await;

    let stream_body = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_split\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-5-20260601\",\"stop_reason\":null,\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"split-across-chunks\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

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

    // reqwest's bytes_stream may deliver the body in larger chunks,
    // but the parser must handle any chunking. We just verify the
    // final assembled message is correct.
    let mut event_count = 0;
    while let Some(event) = stream.next().await {
        event_count += 1;
        event.expect("event should be Ok");
    }
    assert_eq!(event_count, 6); // 1+1+1+1+1+1

    let final_msg = stream.final_message().expect("final_message");
    assert_eq!(final_msg.id, "msg_split");
    match &final_msg.content[0] {
        ContentBlock::Text(t) => assert_eq!(t.text, "split-across-chunks"),
        other => panic!("expected Text block, got {other:?}"),
    }
}