use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use sylvander_agent::bus::{
    BusMessage, MessageBus, MessageKind, Recipient, Sender, StreamEvent, SubscriptionFilter,
};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tracing::error;

use super::{EvidenceError, EvidenceEvent, EvidenceStore, StepStart, TurnStart};
use crate::config::EvidenceContentPolicy;

/// Records bus activity into a durable evidence store and drains on shutdown.
pub struct EvidenceRecorder {
    run_id: String,
    store: EvidenceStore,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl EvidenceRecorder {
    pub async fn start(
        bus: Arc<dyn MessageBus>,
        store: EvidenceStore,
        server_name: String,
        content: EvidenceContentPolicy,
        retention_days: u32,
    ) -> Result<Self, EvidenceError> {
        let retention_seconds = i64::from(retention_days).saturating_mul(86_400);
        store
            .prune_before(now_secs().saturating_sub(retention_seconds))
            .await?;
        let run_id = uuid::Uuid::new_v4().to_string();
        store
            .start_run(run_id.clone(), server_name, now_secs())
            .await?;
        let mut receiver = bus
            .subscribe(SubscriptionFilter::all())
            .await
            .map_err(|error| EvidenceError::Subscribe(error.to_string()))?;
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let task_store = store.clone();
        let task_run_id = run_id.clone();
        let task = tokio::spawn(async move {
            let mut active_turns = HashMap::new();
            loop {
                tokio::select! {
                    Some(message) = receiver.recv() => {
                        record_message(&task_store, &task_run_id, content, &mut active_turns, message).await;
                    }
                    _ = &mut stop_rx => {
                        while let Ok(message) = receiver.try_recv() {
                            record_message(&task_store, &task_run_id, content, &mut active_turns, message).await;
                        }
                        let ended_at = now_secs();
                        for turn_id in active_turns.into_values() {
                            if let Err(error) = task_store
                                .finish_turn(turn_id, ended_at, "interrupted", 0)
                                .await
                            {
                                error!(%error, "failed to close active turn during shutdown");
                            }
                        }
                        break;
                    }
                }
            }
        });
        Ok(Self {
            run_id,
            store,
            stop: Mutex::new(Some(stop_tx)),
            task: Mutex::new(Some(task)),
        })
    }

    #[must_use]
    pub fn store(&self) -> EvidenceStore {
        self.store.clone()
    }

    pub async fn shutdown(&self) -> Result<(), EvidenceError> {
        if let Some(stop) = self.stop.lock().await.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.lock().await.take() {
            task.await
                .map_err(|error| EvidenceError::Task(error.to_string()))?;
        }
        self.store
            .finish_run(self.run_id.clone(), now_secs(), "completed")
            .await
    }
}

async fn record_message(
    store: &EvidenceStore,
    run_id: &str,
    content: EvidenceContentPolicy,
    active_turns: &mut HashMap<String, String>,
    message: BusMessage,
) {
    if let Err(error) = record_message_inner(store, run_id, content, active_turns, message).await {
        error!(%error, "failed to persist runtime evidence");
    }
}

async fn record_message_inner(
    store: &EvidenceStore,
    run_id: &str,
    content: EvidenceContentPolicy,
    active_turns: &mut HashMap<String, String>,
    message: BusMessage,
) -> Result<(), EvidenceError> {
    let session_id = message.session_id.to_string();
    let event_type = event_type(&message);
    let serialized = serde_json::to_vec(&message)
        .map_err(|error| EvidenceError::Serialize(error.to_string()))?;
    let digest = sha256(&serialized);
    let turn_id = match (&message.sender, &message.kind) {
        (Sender::User(_), MessageKind::Chat) => {
            let id = format!("turn:{}", message.id.0);
            let agent_id = match &message.recipient {
                Recipient::Agent(agent_id) => Some(agent_id.to_string()),
                Recipient::Broadcast => None,
            };
            store
                .start_turn(TurnStart {
                    id: id.clone(),
                    run_id: run_id.to_string(),
                    session_id: session_id.clone(),
                    agent_id,
                    started_at: message.timestamp,
                    input_bytes: message.payload.len() as u64,
                    input_digest: Some(sha256(message.payload.as_bytes())),
                })
                .await?;
            active_turns.insert(session_id.clone(), id.clone());
            Some(id)
        }
        _ => active_turns.get(&session_id).cloned(),
    };

    if let (Some(turn_id), MessageKind::Stream(stream)) = (&turn_id, &message.kind) {
        record_stream(store, turn_id, &message, stream).await?;
        if let Some((status, success, output_bytes)) = terminal(stream) {
            store
                .record_outcome(
                    format!("outcome:{}", message.id.0),
                    turn_id.clone(),
                    event_type.clone(),
                    success,
                    message.timestamp,
                )
                .await?;
            store
                .finish_turn(turn_id.clone(), message.timestamp, status, output_bytes)
                .await?;
            active_turns.remove(&session_id);
        }
    }

    let payload_json = match content {
        EvidenceContentPolicy::MetadataOnly => None,
        EvidenceContentPolicy::Redacted => Some(
            serde_json::json!({
                "event_type": event_type,
                "payload": "[REDACTED]",
                "attachments": message.attachments.len()
            })
            .to_string(),
        ),
        EvidenceContentPolicy::Full => String::from_utf8(serialized.clone()).ok(),
    };
    store
        .append_event(EvidenceEvent {
            id: message.id.0.to_string(),
            run_id: run_id.to_string(),
            session_id,
            turn_id,
            event_type,
            occurred_at: message.timestamp,
            observed_at: now_secs(),
            payload_bytes: serialized.len() as u64,
            payload_digest: Some(digest),
            payload_json,
            privacy_class: privacy_class(&message.kind).into(),
        })
        .await
}

async fn record_stream(
    store: &EvidenceStore,
    turn_id: &str,
    message: &BusMessage,
    stream: &StreamEvent,
) -> Result<(), EvidenceError> {
    match stream {
        StreamEvent::ToolCall {
            call_id,
            tool_name,
            input,
        } => {
            let bytes = serde_json::to_vec(input)
                .map_err(|error| EvidenceError::Serialize(error.to_string()))?;
            store
                .start_step(StepStart {
                    id: call_id.clone(),
                    turn_id: turn_id.to_string(),
                    kind: "tool".into(),
                    name: tool_name.clone(),
                    started_at: message.timestamp,
                    input_bytes: bytes.len() as u64,
                    input_digest: Some(sha256(&bytes)),
                })
                .await
        }
        StreamEvent::ToolResult {
            call_id,
            output,
            is_error,
            ..
        } => {
            store
                .finish_step(
                    call_id.clone(),
                    message.timestamp,
                    if *is_error { "failed" } else { "succeeded" },
                    output.len() as u64,
                )
                .await
        }
        _ => Ok(()),
    }
}

fn terminal(event: &StreamEvent) -> Option<(&'static str, bool, u64)> {
    match event {
        StreamEvent::Done { text } => Some(("succeeded", true, text.len() as u64)),
        StreamEvent::TurnInterrupted { .. } => Some(("interrupted", false, 0)),
        _ => None,
    }
}

fn event_type(message: &BusMessage) -> String {
    match &message.kind {
        MessageKind::Chat => match message.sender {
            Sender::User(_) => "user_chat".into(),
            Sender::Agent(_) => "agent_chat".into(),
            Sender::System => "system_chat".into(),
        },
        MessageKind::System(event) => format!("system_{}", snake_name(event)),
        MessageKind::Stream(event) => format!("stream_{}", snake_name(event)),
    }
}

fn snake_name(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(|kind| kind.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".into())
}

fn privacy_class(kind: &MessageKind) -> &'static str {
    match kind {
        MessageKind::System(_) => "operational",
        MessageKind::Chat | MessageKind::Stream(_) => "user_content",
    }
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn now_secs() -> i64 {
    sylvander_agent::session::now_secs()
}

#[cfg(test)]
mod tests {
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
        bus.publish(BusMessage::user_chat(
            SessionId::new("session-1"),
            "user",
            "read it",
        ))
        .await
        .unwrap();
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
        assert_eq!(counts.events, 4);
    }
}
