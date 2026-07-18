use super::*;
use sylvander_agent::bus::{InProcessMessageBus, MessageId};
use sylvander_agent::spec::{AgentId, SessionId};

fn stream_message(event: StreamEvent) -> BusMessage {
    BusMessage {
        session_id: SessionId::new("session-1"),
        sender: Sender::Agent(AgentId::new("agent-1")),
        recipient: Recipient::Broadcast,
        kind: MessageKind::Stream(event),
        payload: String::new(),
        attachments: Vec::new(),
        timestamp: now_secs(),
        id: MessageId::new(),
    }
}

#[tokio::test]
async fn records_a_turn_without_raw_metadata_only_content() {
    let bus = Arc::new(InProcessMessageBus::new());
    let store = EvidenceStore::open_in_memory().await.unwrap();
    let recorder = EvidenceRecorder::start(
        bus.clone(),
        store.clone(),
        "test".into(),
        EvidenceContentPolicy::MetadataOnly,
        30,
    )
    .await
    .unwrap();
    bus.publish(BusMessage::user_chat(
        "session-1".into(),
        "user",
        "secret prompt",
    ))
    .await
    .unwrap();
    recorder.shutdown().await.unwrap();

    let counts = store.counts().await.unwrap();
    assert_eq!(counts.runs, 1);
    assert_eq!(counts.turns, 1);
    assert_eq!(counts.events, 1);
}

#[tokio::test]
async fn normalizes_tool_steps_and_terminal_outcome() {
    let bus = Arc::new(InProcessMessageBus::new());
    let store = EvidenceStore::open_in_memory().await.unwrap();
    let recorder = EvidenceRecorder::start(
        bus.clone(),
        store.clone(),
        "test".into(),
        EvidenceContentPolicy::MetadataOnly,
        30,
    )
    .await
    .unwrap();
    let user_message = BusMessage::user_chat(SessionId::new("session-1"), "user", "read it");
    let user_message_id = user_message.id.0;
    bus.publish(user_message).await.unwrap();
    bus.publish(stream_message(StreamEvent::ToolCall {
        call_id: "call-1".into(),
        tool_name: "read".into(),
        input: serde_json::json!({"path":"secret.txt"}),
    }))
    .await
    .unwrap();
    bus.publish(stream_message(StreamEvent::ToolResult {
        call_id: "call-1".into(),
        tool_name: "read".into(),
        output: "content".into(),
        is_error: false,
    }))
    .await
    .unwrap();
    bus.publish(stream_message(StreamEvent::IterationEnd {
        iteration: 1,
        input_tokens: 13,
        output_tokens: 8,
        cost_nano_usd: Some(21),
    }))
    .await
    .unwrap();
    bus.publish(stream_message(StreamEvent::Done {
        text: "complete".into(),
    }))
    .await
    .unwrap();
    recorder.shutdown().await.unwrap();

    let counts = store.counts().await.unwrap();
    assert_eq!(counts.turns, 1);
    assert_eq!(counts.steps, 1);
    assert_eq!(counts.outcomes, 1);
    assert_eq!(counts.events, 5);
    let usage = recorder
        .store()
        .turn_usage(format!("turn:{user_message_id}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(usage.input_tokens, 13);
    assert_eq!(usage.output_tokens, 8);
    assert_eq!(usage.cost_nano_usd, Some(21));
    assert_eq!(usage.iteration_count, 1);
}
