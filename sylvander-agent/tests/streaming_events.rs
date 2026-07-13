//! End-to-end tests for streaming events on the bus.
//!
//! Verifies that `AgentRun::handle_message` publishes `StreamEvent`
//! variants (TextDelta, ToolCall, ToolResult, Done, etc.) to the bus
//! in real-time as the loop executes.

use std::sync::Arc;

use serde_json::json;
use sylvander_agent::bus::{PlanDecision, StreamEvent};
use sylvander_agent::prelude::*;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::ModelCapabilities;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// --- helpers ---

fn mock_client(server: &MockServer) -> AnthropicClient {
    AnthropicClient::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .build()
        .expect("client build")
}

async fn build_agent(server: &MockServer) -> (AgentRun, Arc<InProcessMessageBus>, SessionId) {
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

#[tokio::test]
async fn session_interrupt_cancels_one_active_turn_and_emits_terminal_event() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(5))
                .set_body_json(json!({
                    "id": "msg_slow",
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "text", "text": "too late"}],
                    "model": "claude-sonnet-5-20260601",
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 10, "output_tokens": 2}
                })),
        )
        .mount(&server)
        .await;

    let bus = Arc::new(InProcessMessageBus::new());
    let spec = AgentSpec::builder()
        .id("interrupt-test")
        .name("Interrupt Test")
        .model_name("claude-sonnet-5-20260601")
        .build()
        .expect("spec");
    let run = AgentRun::builder(spec, mock_client(&server))
        .bus(bus.clone())
        .build()
        .expect("build");
    let agent_id = run.id().clone();
    let inbox = bus
        .subscribe(run.subscription_filter())
        .await
        .expect("agent inbox");
    let task = tokio::spawn(run.run(inbox));
    let mut events = subscribe_stream(&bus).await;
    let session_id = SessionId::new("interrupt-session");
    bus.publish(BusMessage::system_join_session(
        agent_id.clone(),
        session_id.clone(),
        SessionMetadata {
            workspace: "/tmp".into(),
            name: "interrupt".into(),
            user_id: "user-1".into(),
        },
    ))
    .await
    .expect("join");
    bus.publish(BusMessage::user_chat(session_id.clone(), "user-1", "wait"))
        .await
        .expect("chat");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    bus.publish(BusMessage::system_interrupt_turn(
        agent_id.clone(),
        session_id.clone(),
    ))
    .await
    .expect("interrupt");

    let interrupted = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let message = events.recv().await.expect("event stream");
            if message.session_id == session_id
                && matches!(
                    message.kind,
                    MessageKind::Stream(StreamEvent::TurnInterrupted { .. })
                )
            {
                break;
            }
        }
    })
    .await;
    assert!(
        interrupted.is_ok(),
        "interrupt must not wait for the LLM response"
    );

    bus.publish(BusMessage::system_stop(agent_id))
        .await
        .expect("stop");
    task.await.expect("agent task");
}

/// Collect stream events into a vec of variant names
fn event_names(events: &[BusMessage]) -> Vec<String> {
    events
        .iter()
        .filter_map(|m| match &m.kind {
            MessageKind::Stream(ev) => Some(match ev {
                StreamEvent::TextDelta { .. } => "TextDelta",
                StreamEvent::ThinkingDelta { .. } => "ThinkingDelta",
                StreamEvent::ModelRetry { .. } => "ModelRetry",
                StreamEvent::InteractionTimedOut { .. } => "InteractionTimedOut",
                StreamEvent::CompactionStarted { .. } => "CompactionStarted",
                StreamEvent::CompactionCompleted { .. } => "CompactionCompleted",
                StreamEvent::CompactionFailed { .. } => "CompactionFailed",
                StreamEvent::ToolCall { .. } => "ToolCall",
                StreamEvent::ToolOutputDelta { .. } => "ToolOutputDelta",
                StreamEvent::ToolResult { .. } => "ToolResult",
                StreamEvent::IterationStart { .. } => "IterationStart",
                StreamEvent::IterationEnd { .. } => "IterationEnd",
                StreamEvent::Done { .. } => "Done",
                StreamEvent::ToolApprovalRequired { .. } => "ToolApprovalRequired",
                StreamEvent::AskUser { .. } => "AskUser",
                StreamEvent::UserAnswer { .. } => "UserAnswer",
                StreamEvent::TurnInterrupted { .. } => "TurnInterrupted",
                StreamEvent::PlanProposed { .. } => "PlanProposed",
                StreamEvent::PlanUpdated { .. } => "PlanUpdated",
                StreamEvent::TaskStarted { .. } => "TaskStarted",
                StreamEvent::TaskProgress { .. } => "TaskProgress",
                StreamEvent::TaskCompleted { .. } => "TaskCompleted",
                StreamEvent::TaskFailed { .. } => "TaskFailed",
                StreamEvent::TaskCancelled { .. } => "TaskCancelled",
            }),
            _ => None,
        })
        .map(String::from)
        .collect()
}

#[tokio::test]
async fn proposed_plan_blocks_until_typed_resolution_then_continues() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_plan",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "content": [{
                "type": "tool_use",
                "id": "plan_001",
                "name": "present_plan",
                "input": {"steps": ["inspect", "implement", "verify"]}
            }],
            "usage": {"input_tokens": 10, "output_tokens": 8}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_update",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "content": [{
                "type": "tool_use",
                "id": "plan_update_1",
                "name": "update_plan",
                "input": {
                    "plan_id": "plan_001",
                    "steps": ["inspect", "implement", "verify"],
                    "current": 1
                }
            }],
            "usage": {"input_tokens": 15, "output_tokens": 5}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_done",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "Starting the approved work."}],
            "usage": {"input_tokens": 18, "output_tokens": 5}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let bus = Arc::new(InProcessMessageBus::new());
    let spec = AgentSpec::builder()
        .id("plan-test")
        .name("Plan Test")
        .model_name("claude-sonnet-5-20260601")
        .build()
        .expect("spec");
    let tools = ToolRegistry::new()
        .register(PresentPlanTool::new())
        .register(UpdatePlanTool::new());
    let run = AgentRun::builder(spec, mock_client(&server))
        .bus(bus.clone())
        .model_capabilities(ModelCapabilities::TOOL_USE)
        .override_tools(tools)
        .build()
        .expect("build");
    let agent_id = run.id().clone();
    let inbox = bus
        .subscribe(run.subscription_filter())
        .await
        .expect("inbox");
    let task = tokio::spawn(run.run(inbox));
    let mut events = subscribe_stream(&bus).await;
    let sid = SessionId::new("plan-session");
    bus.publish(BusMessage::system_join_session(
        agent_id.clone(),
        sid.clone(),
        SessionMetadata {
            workspace: "/tmp".into(),
            name: "plan".into(),
            user_id: "user-1".into(),
        },
    ))
    .await
    .expect("join");
    bus.publish(BusMessage::user_chat(sid.clone(), "user-1", "make a plan"))
        .await
        .expect("chat");

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let message = events.recv().await.expect("event");
            if matches!(
                message.kind,
                MessageKind::Stream(StreamEvent::PlanProposed { .. })
            ) {
                break;
            }
        }
    })
    .await
    .expect("plan proposal");

    bus.publish(BusMessage {
        session_id: sid.clone(),
        sender: Sender::System,
        recipient: Recipient::Agent(agent_id.clone()),
        kind: MessageKind::System(SystemMessage::ResolvePlan {
            plan_id: "plan_001".into(),
            decision: PlanDecision::Approved,
        }),
        payload: String::new(),
        attachments: Vec::new(),
        timestamp: sylvander_agent::session::now_secs(),
        id: MessageId::new(),
    })
    .await
    .expect("resolve");

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let message = events.recv().await.expect("event");
            if matches!(
                message.kind,
                MessageKind::Stream(StreamEvent::PlanUpdated { current: 1, .. })
            ) {
                break;
            }
        }
    })
    .await
    .expect("plan progress update");

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let message = events.recv().await.expect("event");
            if matches!(message.kind, MessageKind::Stream(StreamEvent::Done { .. })) {
                break;
            }
        }
    })
    .await
    .expect("done after approval");
    bus.publish(BusMessage::system_stop(agent_id))
        .await
        .expect("stop");
    task.await.expect("agent task");
}

#[tokio::test]
async fn background_task_is_real_read_only_work_and_cancels_independently() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "messages": [{"role": "user", "content": "delegate"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_spawn",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "tool_use",
            "content": [{
                "type": "tool_use",
                "id": "task_tool_1",
                "name": "start_background_task",
                "input": {"purpose": "inspect", "prompt": "inspect only"}
            }],
            "usage": {"input_tokens": 10, "output_tokens": 8}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "messages": [{"role": "user", "content": "inspect only"}]
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(5))
                .set_body_json(json!({
                    "id": "msg_background",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-5-20260601",
                    "stop_reason": "end_turn",
                    "content": [{"type": "text", "text": "late result"}],
                    "usage": {"input_tokens": 10, "output_tokens": 3}
                })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_main_done",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "Main work continues."}],
            "usage": {"input_tokens": 12, "output_tokens": 4}
        })))
        .mount(&server)
        .await;

    let bus = Arc::new(InProcessMessageBus::new());
    let spec = AgentSpec::builder()
        .id("background-test")
        .name("Background Test")
        .model_name("claude-sonnet-5-20260601")
        .build()
        .expect("spec");
    let tools = ToolRegistry::new().register(StartBackgroundTaskTool::new());
    let run = AgentRun::builder(spec, mock_client(&server))
        .bus(bus.clone())
        .model_capabilities(ModelCapabilities::TOOL_USE)
        .override_tools(tools)
        .build()
        .expect("build");
    let agent_id = run.id().clone();
    let inbox = bus
        .subscribe(run.subscription_filter())
        .await
        .expect("inbox");
    let task = tokio::spawn(run.run(inbox));
    let mut events = subscribe_stream(&bus).await;
    let sid = SessionId::new("background-session");
    bus.publish(BusMessage::system_join_session(
        agent_id.clone(),
        sid.clone(),
        SessionMetadata {
            workspace: "/tmp".into(),
            name: "background".into(),
            user_id: "user-1".into(),
        },
    ))
    .await
    .expect("join");
    bus.publish(BusMessage::user_chat(sid.clone(), "user-1", "delegate"))
        .await
        .expect("chat");

    let expected_task_id = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let message = events.recv().await.expect("event");
            if let MessageKind::Stream(StreamEvent::TaskStarted { task_id, .. }) = message.kind {
                break task_id;
            }
        }
    })
    .await
    .expect("task start");
    bus.publish(BusMessage {
        session_id: sid.clone(),
        sender: Sender::System,
        recipient: Recipient::Agent(agent_id.clone()),
        kind: MessageKind::System(SystemMessage::CancelTask {
            session_id: sid.clone(),
            task_id: expected_task_id.clone(),
        }),
        payload: String::new(),
        attachments: Vec::new(),
        timestamp: sylvander_agent::session::now_secs(),
        id: MessageId::new(),
    })
    .await
    .expect("cancel");

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let message = events.recv().await.expect("event");
            if matches!(
                message.kind,
                MessageKind::Stream(StreamEvent::TaskCancelled { ref task_id, .. })
                    if task_id == &expected_task_id
            ) {
                break;
            }
        }
    })
    .await
    .expect("task cancellation");
    bus.publish(BusMessage::system_stop(agent_id))
        .await
        .expect("stop");
    task.await.expect("agent task");
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
    assert!(
        names.contains(&"IterationStart".into()),
        "expected IterationStart, got {names:?}"
    );
    assert!(
        names.contains(&"Done".into()),
        "expected Done, got {names:?}"
    );
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
    assert!(
        names.contains(&"Done".into()),
        "expected Done, got {names:?}"
    );
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
    assert_eq!(
        ctx.len(),
        2,
        "history should have user + assistant message, not chunks"
    );
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
