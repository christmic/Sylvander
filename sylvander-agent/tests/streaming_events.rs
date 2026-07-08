//! End-to-end tests for streaming events on the bus.
//!
//! Verifies that `AgentRun::handle_message` publishes `StreamEvent`
//! variants (TextDelta, ToolCall, ToolResult, Done, etc.) to the bus
//! in real-time as the loop executes.

use std::sync::Arc;

use serde_json::json;
use sylvander_agent::prelude::*;
use sylvander_agent::bus::StreamEvent;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// --- helpers ---

fn mock_client(server: &MockServer) -> AnthropicClient {
    AnthropicClient::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .build()
        .expect("client build")
}

async fn build_agent(
    server: &MockServer,
) -> (AgentRun, Arc<InProcessMessageBus>, SessionId) {
    let bus = Arc::new(InProcessMessageBus::new());
    let spec = AgentSpec::builder()
        .id("stream-test")
        .name("Stream Test")
        .model_name("claude-sonnet-5-20260601")
        .build()
        .expect("spec");

    let run = AgentRun::builder(spec, mock_client(server))
        .bus(bus.clone())
        .build()
        .expect("build");

    let sid = run
        .join_session(SessionMetadata {
            workspace: "/tmp".into(),
            name: "test".into(),
            user_id: "user-1".into(),
        })
        .await;

    (run, bus, sid)
}

/// Subscribe to all stream events for the agent
async fn subscribe_stream(
    bus: &InProcessMessageBus,
) -> tokio::sync::mpsc::UnboundedReceiver<BusMessage> {
    bus.subscribe(SubscriptionFilter::all())
        .await
        .expect("subscribe")
}

/// Collect stream events into a vec of variant names
fn event_names(events: &[BusMessage]) -> Vec<String> {
    events
        .iter()
        .filter_map(|m| match &m.kind {
            MessageKind::Stream(ev) => Some(match ev {
                StreamEvent::TextDelta { .. } => "TextDelta",
                StreamEvent::ThinkingDelta { .. } => "ThinkingDelta",
                StreamEvent::ToolCall { .. } => "ToolCall",
                StreamEvent::ToolResult { .. } => "ToolResult",
                StreamEvent::IterationStart { .. } => "IterationStart",
                StreamEvent::IterationEnd { .. } => "IterationEnd",
                StreamEvent::Done { .. } => "Done",
            }),
            _ => None,
        })
        .map(String::from)
        .collect()
}

// --- tests ---

#[tokio::test]
async fn text_deltas_published_to_bus() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello world"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    let (agent, bus, sid) = build_agent(&server).await;
    let mut rx = subscribe_stream(&bus).await;

    let msg = BusMessage::user_chat(sid, "user-1", "Hi");
    agent.handle_message(msg).await.expect("handle_message");

    // Drain events
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev.kind, MessageKind::Stream(_)) {
            events.push(ev);
        }
    }

    let names = event_names(&events);
    assert!(names.contains(&"IterationStart".into()), "expected IterationStart, got {names:?}");
    assert!(names.contains(&"Done".into()), "expected Done, got {names:?}");
}

#[tokio::test]
async fn tool_call_events_published() {
    let server = MockServer::start().await;

    // First response: tool_use
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "content": [{
                "type": "tool_use",
                "id": "toolu_001",
                "name": "mock_tool",
                "input": {"query": "test"}
            }],
            "usage": {"input_tokens": 10, "output_tokens": 8}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second response: text after tool result
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_2",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "Tool result processed"}],
            "usage": {"input_tokens": 15, "output_tokens": 5}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let bus = Arc::new(InProcessMessageBus::new());

    // Build agent with a mock tool
    let mock_tool = MockTool::new("mock_tool", "A test tool", ToolOutput::ok("mock result"));
    let tools = ToolRegistry::new().register(mock_tool);

    let spec = AgentSpec::builder()
        .id("tool-test")
        .name("Tool Test")
        .model_name("claude-sonnet-5-20260601")
        .build()
        .expect("spec");

    let agent = AgentRun::builder(spec, mock_client(&server))
        .bus(bus.clone())
        .model_capabilities(ModelCapabilities::TOOL_USE)
        .override_tools(tools)
        .build()
        .expect("build");

    agent
        .join_session(SessionMetadata {
            workspace: "/tmp".into(),
            name: "test".into(),
            user_id: "user-1".into(),
        })
        .await;

    let mut rx = subscribe_stream(&bus).await;

    let sid = agent.list_sessions().await[0].clone();
    let msg = BusMessage::user_chat(sid, "user-1", "Run tool");
    agent.handle_message(msg).await.expect("handle_message");

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev.kind, MessageKind::Stream(_)) {
            events.push(ev);
        }
    }

    let names = event_names(&events);
    assert!(
        names.contains(&"ToolCall".into()),
        "expected ToolCall, got {names:?}"
    );
    assert!(
        names.contains(&"ToolResult".into()),
        "expected ToolResult, got {names:?}"
    );
    assert!(names.contains(&"Done".into()), "expected Done, got {names:?}");
}

#[tokio::test]
async fn session_history_contains_complete_message_not_chunks() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Complete response"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 3}
        })))
        .mount(&server)
        .await;

    let (agent, _bus, sid) = build_agent(&server).await;

    let msg = BusMessage::user_chat(sid.clone(), "user-1", "Hi");
    agent.handle_message(msg).await.expect("handle_message");

    // Check session history
    let ctx = agent.get_session(&sid).await.expect("session should exist");
    assert_eq!(ctx.len(), 2, "history should have user + assistant message, not chunks");
}

#[tokio::test]
async fn done_event_contains_full_text() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Final answer"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 2}
        })))
        .mount(&server)
        .await;

    let (agent, bus, sid) = build_agent(&server).await;
    let mut rx = subscribe_stream(&bus).await;

    let msg = BusMessage::user_chat(sid, "user-1", "Q");
    agent.handle_message(msg).await.expect("handle_message");

    let mut done_event = None;
    while let Ok(ev) = rx.try_recv() {
        if let MessageKind::Stream(StreamEvent::Done { text }) = &ev.kind {
            done_event = Some(text.clone());
        }
    }

    assert_eq!(done_event.as_deref(), Some("Final answer"));
}

#[tokio::test]
async fn agent_error_published_and_returns_err() {
    let server = MockServer::start().await;

    // Return a 500 to trigger an LLM error
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&server)
        .await;

    let (agent, bus, sid) = build_agent(&server).await;
    let mut rx = subscribe_stream(&bus).await;

    let msg = BusMessage::user_chat(sid, "user-1", "Hi");
    let result = agent.handle_message(msg).await;

    // Should return error
    assert!(result.is_err());

    // An error Chat message should have been published
    let mut found_error = false;
    while let Ok(ev) = rx.try_recv() {
        if let MessageKind::Chat = ev.kind {
            if ev.payload.contains("Error") {
                found_error = true;
            }
        }
    }
    assert!(found_error, "expected an error Chat message on the bus");
}
