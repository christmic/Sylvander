//! Durable background work submitted to the governed Agent mailbox.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use sylvander_agent::task_gate::{BackgroundTaskRequest, TaskGate};
use sylvander_api::{
    AgentId, AgentInstanceId, BusMessage, CoordinationMessageId, SessionId, StreamEvent, TaskId,
};
use sylvander_channel::MessageBus;

use crate::coordination::governance::GovernancePolicy;
use crate::coordination::mailbox::{BACKGROUND_TASK_TTL_SECONDS, CoordinationMessageKind};
use crate::coordination::service::{
    CoordinationService, CreateTaskRequest, DEFAULT_ARBITRATION_TTL_SECONDS,
    DispatchMessageOutcome, DispatchMessageRequest,
};
use crate::observability::{RuntimeCoordinationOutcome, RuntimeEvent, RuntimeObservability};
use crate::session::now_secs;
use crate::storage::session::SqliteSessionStore;

const BACKGROUND_TASK_TOKEN_BUDGET: u64 = 20_000;

pub(super) struct BusTaskGate {
    pub(super) bus: Arc<dyn MessageBus>,
    pub(super) agent_id: AgentId,
    pub(super) agent_instance_id: AgentInstanceId,
    pub(super) session_id: SessionId,
    pub(super) store: Option<Arc<SqliteSessionStore>>,
    pub(super) observability: RuntimeObservability,
}

#[async_trait::async_trait]
impl TaskGate for BusTaskGate {
    async fn start(&self, request: BackgroundTaskRequest) -> Result<String, String> {
        if request.prompt.trim().is_empty() {
            return Err("background task prompt cannot be empty".into());
        }
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| "durable background task runtime is unavailable".to_owned())?;
        let task_id = TaskId::new(stable_id(
            "background-task",
            &self.session_id,
            &self.agent_instance_id,
            &request.invocation_id,
        ));
        let message_id = CoordinationMessageId::new(stable_id(
            "background-message",
            &self.session_id,
            &self.agent_instance_id,
            &request.invocation_id,
        ));
        let service = CoordinationService::new(
            store.clone(),
            GovernancePolicy::default(),
            DEFAULT_ARBITRATION_TTL_SECONDS,
        );
        let now = now_secs();
        let prompt = format!(
            "[durable background task; task_id={}]\nPurpose: {}\n\n{}\n\nUse manage_workflow to claim this task before work and commit its final state.",
            task_id.0, request.purpose, request.prompt
        );
        let task = service
            .create_task(
                CreateTaskRequest {
                    task_id: task_id.clone(),
                    session_id: self.session_id.clone(),
                    parent_task_id: None,
                    created_by: self.agent_instance_id.clone(),
                    assigned_to: self.agent_instance_id.clone(),
                    objective: prompt.clone(),
                    token_budget: BACKGROUND_TASK_TOKEN_BUDGET,
                    max_handoffs: 0,
                },
                now,
            )
            .await
            .map_err(|error| error.to_string())?;
        let expires_at = task
            .created_at
            .checked_add(BACKGROUND_TASK_TTL_SECONDS)
            .ok_or_else(|| "background task deadline overflow".to_owned())?;
        let outcome = service
            .dispatch_message(
                DispatchMessageRequest {
                    message_id,
                    session_id: self.session_id.clone(),
                    sender_instance_id: self.agent_instance_id.clone(),
                    recipient_instance_id: self.agent_instance_id.clone(),
                    task_id: Some(task_id.clone()),
                    kind: CoordinationMessageKind::Task,
                    payload: prompt,
                    max_hops: 1,
                    expires_at,
                },
                now,
            )
            .await
            .map_err(|error| error.to_string())?;
        let coordination_outcome = match outcome {
            DispatchMessageOutcome::Enqueued(_) => RuntimeCoordinationOutcome::Enqueued,
            DispatchMessageOutcome::EnqueuedByModerator { .. } => {
                RuntimeCoordinationOutcome::ModeratorAuthorized
            }
            DispatchMessageOutcome::RequiresArbitration { .. } => {
                RuntimeCoordinationOutcome::ArbitrationRequired
            }
            DispatchMessageOutcome::RejectedByModerator { .. } => {
                return Err("background task was rejected by the moderator".into());
            }
        };
        self.observability
            .record(RuntimeEvent::CoordinationTransition {
                session_id: self.session_id.clone(),
                outcome: RuntimeCoordinationOutcome::TaskCreated,
            });
        self.observability
            .record(RuntimeEvent::CoordinationTransition {
                session_id: self.session_id.clone(),
                outcome: coordination_outcome,
            });
        let _ = self
            .bus
            .publish(BusMessage::stream_event(
                self.session_id.clone(),
                self.agent_id.clone(),
                StreamEvent::TaskStarted {
                    task_id: task_id.0.clone(),
                    owner: self.agent_instance_id.0.clone(),
                    purpose: request.purpose,
                },
            ))
            .await;
        Ok(task_id.0)
    }
}

fn stable_id(
    domain: &str,
    session_id: &SessionId,
    instance_id: &AgentInstanceId,
    invocation_id: &str,
) -> String {
    format!(
        "{domain}:{:x}",
        Sha256::digest(
            [
                session_id.0.as_bytes(),
                instance_id.0.as_bytes(),
                invocation_id.as_bytes(),
            ]
            .concat()
        )
    )
}
