use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};
use sylvander_agent::bus::{
    BusMessage, MessageBus, MessageKind, Recipient, Sender, StreamEvent, SubscriptionFilter,
};
use tokio::sync::{Mutex, RwLock, oneshot};
use tokio::task::JoinHandle;
use tracing::error;

use super::{
    EvidenceClassification, EvidenceError, EvidenceEvent, EvidenceStore, GovernedRecordInput,
    GovernedRecordKind, StepStart, TurnStart, structured_redact,
};
use crate::config::EvidenceContentPolicy;

/// Records bus activity into a durable evidence store and drains on shutdown.
pub struct EvidenceRecorder {
    run_id: String,
    store: EvidenceStore,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<JoinHandle<()>>>,
    last_error: Arc<RwLock<Option<EvidenceRecorderFailure>>>,
    #[cfg(test)]
    fail_next_record: Arc<AtomicBool>,
}

/// Content-safe recorder failure visible to operational health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceRecorderFailure {
    PersistEvent,
    CloseActiveTurn,
}

impl EvidenceRecorder {
    pub async fn start(
        bus: Arc<dyn MessageBus>,
        store: EvidenceStore,
        server_name: String,
        content: EvidenceContentPolicy,
        retention_days: u32,
    ) -> Result<Self, EvidenceError> {
        if content != EvidenceContentPolicy::MetadataOnly && !store.governance_enabled() {
            return Err(EvidenceError::EncryptionRequired);
        }
        let retention_seconds = i64::from(retention_days).saturating_mul(86_400);
        store
            .prune_before(now_secs().saturating_sub(retention_seconds))
            .await?;
        if store.governance_enabled() {
            store.sweep_governed_retention(now_secs(), 1_000).await?;
        }
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
        let last_error = Arc::new(RwLock::new(None));
        let task_last_error = last_error.clone();
        #[cfg(test)]
        let fail_next_record = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        let task_fail_next_record = fail_next_record.clone();
        let task = tokio::spawn(async move {
            let mut active_turns = HashMap::new();
            loop {
                tokio::select! {
                    Some(message) = receiver.recv() => {
                        #[cfg(test)]
                        let injected = task_fail_next_record.swap(false, Ordering::AcqRel);
                        #[cfg(not(test))]
                        let injected = false;
                        record_message(
                            &task_store,
                            &task_run_id,
                            content,
                            &mut active_turns,
                            message,
                            &task_last_error,
                            injected,
                        ).await;
                    }
                    _ = &mut stop_rx => {
                        while let Ok(message) = receiver.try_recv() {
                            #[cfg(test)]
                            let injected = task_fail_next_record.swap(false, Ordering::AcqRel);
                            #[cfg(not(test))]
                            let injected = false;
                            record_message(
                                &task_store,
                                &task_run_id,
                                content,
                                &mut active_turns,
                                message,
                                &task_last_error,
                                injected,
                            ).await;
                        }
                        let ended_at = now_secs();
                        for active in active_turns.into_values() {
                            if let Err(error) = task_store
                                .finish_turn(active.turn_id, ended_at, "interrupted", 0)
                                .await
                            {
                                error!(%error, "failed to close active turn during shutdown");
                                *task_last_error.write().await =
                                    Some(EvidenceRecorderFailure::CloseActiveTurn);
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
            last_error,
            #[cfg(test)]
            fail_next_record,
        })
    }

    #[must_use]
    pub fn store(&self) -> EvidenceStore {
        self.store.clone()
    }

    /// Return the active evidence-run identifier used to derive opaque
    /// per-turn feedback targets at ingress.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Return the latest content-safe background persistence failure.
    pub async fn last_error(&self) -> Option<EvidenceRecorderFailure> {
        *self.last_error.read().await
    }

    #[cfg(test)]
    pub(crate) fn fail_next_record_for_test(&self) {
        self.fail_next_record.store(true, Ordering::Release);
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
    active_turns: &mut HashMap<String, ActiveTurn>,
    message: BusMessage,
    last_error: &RwLock<Option<EvidenceRecorderFailure>>,
    injected: bool,
) {
    let result = if injected {
        Err(EvidenceError::Task("injected evidence failure".into()))
    } else {
        record_message_inner(store, run_id, content, active_turns, message).await
    };
    if let Err(error) = result {
        error!(%error, "failed to persist runtime evidence");
        *last_error.write().await = Some(EvidenceRecorderFailure::PersistEvent);
    }
}

async fn record_message_inner(
    store: &EvidenceStore,
    run_id: &str,
    content: EvidenceContentPolicy,
    active_turns: &mut HashMap<String, ActiveTurn>,
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
            let user_id = match &message.sender {
                Sender::User(user_id) => user_id.clone(),
                _ => "__system__".into(),
            };
            active_turns.insert(
                session_id.clone(),
                ActiveTurn {
                    turn_id: id.clone(),
                    user_id,
                },
            );
            Some(id)
        }
        _ => active_turns
            .get(&session_id)
            .map(|active| active.turn_id.clone()),
    };
    let governed_user_id = match &message.sender {
        Sender::User(user_id) => user_id.clone(),
        _ => active_turns
            .get(&session_id)
            .map_or_else(|| "__system__".into(), |active| active.user_id.clone()),
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
            // Raw/redacted content is always routed through the encrypted
            // governance table; the normalized event table stays metadata-only.
            payload_json: None,
            privacy_class: privacy_class(&message.kind).into(),
        })
        .await?;
    if content != EvidenceContentPolicy::MetadataOnly {
        let payload = match content {
            EvidenceContentPolicy::MetadataOnly => unreachable!(),
            EvidenceContentPolicy::Redacted => {
                let value = serde_json::from_slice::<serde_json::Value>(&serialized)
                    .map_err(|error| EvidenceError::Serialize(error.to_string()))?;
                serde_json::to_vec(&structured_redact(&value))
                    .map_err(|error| EvidenceError::Serialize(error.to_string()))?
            }
            EvidenceContentPolicy::Full => serialized,
        };
        store
            .put_governed_record(GovernedRecordInput {
                id: format!("event:{}", message.id.0),
                scope: store.governed_scope(governed_user_id)?,
                kind: GovernedRecordKind::Event,
                classification: classification(&message),
                source_ref: format!("bus-message:{}", message.id.0),
                media_type: "application/json".into(),
                payload,
                created_at: message.timestamp,
            })
            .await?;
    }
    Ok(())
}

struct ActiveTurn {
    turn_id: String,
    user_id: String,
}

async fn record_stream(
    store: &EvidenceStore,
    turn_id: &str,
    message: &BusMessage,
    stream: &StreamEvent,
) -> Result<(), EvidenceError> {
    match stream {
        StreamEvent::IterationEnd {
            input_tokens,
            output_tokens,
            cost_nano_usd,
            ..
        } => {
            store
                .record_iteration_usage(
                    turn_id.to_string(),
                    u64::from(*input_tokens),
                    u64::from(*output_tokens),
                    *cost_nano_usd,
                )
                .await
        }
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
        StreamEvent::Error { message } => Some(("failed", false, message.len() as u64)),
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

fn classification(message: &BusMessage) -> EvidenceClassification {
    if message.attachments.is_empty() {
        match &message.kind {
            MessageKind::System(_) => EvidenceClassification::Operational,
            MessageKind::Chat | MessageKind::Stream(_) => EvidenceClassification::Confidential,
        }
    } else {
        EvidenceClassification::Restricted
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
#[path = "../../tests/unit/evidence_recorder.rs"]
mod tests;
