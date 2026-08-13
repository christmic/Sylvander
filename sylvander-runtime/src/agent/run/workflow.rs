//! Runtime implementation of Agent-authored durable workflow intents.

use std::sync::Arc;

use sylvander_agent::workflow_gate::{
    WorkflowCommand, WorkflowGate, WorkflowReceipt, WorkflowTaskState,
};
use sylvander_api::{AgentInstanceId, SessionId, TaskId};

use crate::coordination::governance::GovernancePolicy;
use crate::coordination::service::{
    ClaimTaskRequest, CoordinationService, CreateTaskRequest, DEFAULT_ARBITRATION_TTL_SECONDS,
    FinishClaimedTaskRequest, TransitionTaskRequest,
};
use crate::coordination::task::CoordinationTaskState;
use crate::observability::{RuntimeCoordinationOutcome, RuntimeEvent, RuntimeObservability};
use crate::session::now_secs;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::SqliteSessionStore;

const WORKFLOW_TASK_LEASE_SECONDS: u64 = 30;

pub(super) struct RuntimeWorkflowGate {
    pub(super) store: Arc<SqliteSessionStore>,
    pub(super) session_id: SessionId,
    pub(super) agent_instance_id: AgentInstanceId,
    pub(super) turn_id: String,
    pub(super) observability: RuntimeObservability,
}

#[async_trait::async_trait]
impl WorkflowGate for RuntimeWorkflowGate {
    async fn apply(&self, command: WorkflowCommand) -> Result<WorkflowReceipt, String> {
        let service = CoordinationService::new(
            self.store.clone(),
            GovernancePolicy::default(),
            DEFAULT_ARBITRATION_TTL_SECONDS,
        );
        let (task, outcome) = match command {
            WorkflowCommand::Create {
                task_id,
                objective,
                token_budget,
                max_handoffs,
            } => {
                let task = service
                    .create_task(
                        CreateTaskRequest {
                            task_id: TaskId::new(task_id),
                            session_id: self.session_id.clone(),
                            parent_task_id: None,
                            created_by: self.agent_instance_id.clone(),
                            assigned_to: self.agent_instance_id.clone(),
                            objective,
                            token_budget,
                            max_handoffs,
                        },
                        now_secs(),
                    )
                    .await
                    .map_err(|error| error.to_string());
                (task, RuntimeCoordinationOutcome::TaskCreated)
            }
            WorkflowCommand::Transition {
                task_id,
                state,
                consumed_tokens,
            } => {
                let task_id = TaskId::new(task_id);
                let next_state = runtime_state(state);
                let current = self
                    .store
                    .task(&task_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let current = current.ok_or_else(|| "unknown coordination task".to_owned())?;
                let execution_boundary = matches!(
                    next_state,
                    CoordinationTaskState::Blocked
                        | CoordinationTaskState::AwaitingReview
                        | CoordinationTaskState::Completed
                        | CoordinationTaskState::Failed
                );
                let task = if next_state == CoordinationTaskState::Running {
                    service
                        .claim_task(
                            ClaimTaskRequest {
                                task_id: task_id.clone(),
                                session_id: self.session_id.clone(),
                                actor: self.agent_instance_id.clone(),
                                claim_owner_id: self.turn_id.clone(),
                                lease_seconds: WORKFLOW_TASK_LEASE_SECONDS,
                            },
                            now_secs(),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    self.store
                        .task(&task_id)
                        .await
                        .map_err(|error| error.to_string())?
                        .ok_or_else(|| "unknown coordination task".to_owned())
                } else if current.state == CoordinationTaskState::Running
                    || (current.state == CoordinationTaskState::Ready && execution_boundary)
                {
                    let lease = service
                        .claim_task(
                            ClaimTaskRequest {
                                task_id,
                                session_id: self.session_id.clone(),
                                actor: self.agent_instance_id.clone(),
                                claim_owner_id: self.turn_id.clone(),
                                lease_seconds: WORKFLOW_TASK_LEASE_SECONDS,
                            },
                            now_secs(),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    service
                        .finish_claimed_task(
                            FinishClaimedTaskRequest {
                                lease,
                                next_state,
                                consumed_tokens,
                            },
                            now_secs(),
                        )
                        .await
                        .map_err(|error| error.to_string())
                } else {
                    service
                        .transition_task(
                            TransitionTaskRequest {
                                task_id,
                                session_id: self.session_id.clone(),
                                actor: self.agent_instance_id.clone(),
                                next_state,
                                consumed_tokens,
                            },
                            now_secs(),
                        )
                        .await
                        .map_err(|error| error.to_string())
                };
                (task, RuntimeCoordinationOutcome::TaskTransitioned)
            }
        };
        let task = task?;
        self.observability
            .record(RuntimeEvent::CoordinationTransition {
                session_id: self.session_id.clone(),
                outcome,
            });
        Ok(WorkflowReceipt {
            task_id: task.task_id.0,
            state: agent_state(task.state),
            revision: task.revision,
        })
    }
}

const fn runtime_state(state: WorkflowTaskState) -> CoordinationTaskState {
    match state {
        WorkflowTaskState::Ready => CoordinationTaskState::Ready,
        WorkflowTaskState::Running => CoordinationTaskState::Running,
        WorkflowTaskState::Blocked => CoordinationTaskState::Blocked,
        WorkflowTaskState::AwaitingReview => CoordinationTaskState::AwaitingReview,
        WorkflowTaskState::Completed => CoordinationTaskState::Completed,
        WorkflowTaskState::Failed => CoordinationTaskState::Failed,
        WorkflowTaskState::Cancelled => CoordinationTaskState::Cancelled,
    }
}

const fn agent_state(state: CoordinationTaskState) -> WorkflowTaskState {
    match state {
        CoordinationTaskState::Proposed | CoordinationTaskState::Ready => WorkflowTaskState::Ready,
        CoordinationTaskState::Running => WorkflowTaskState::Running,
        CoordinationTaskState::Blocked => WorkflowTaskState::Blocked,
        CoordinationTaskState::AwaitingReview => WorkflowTaskState::AwaitingReview,
        CoordinationTaskState::Completed => WorkflowTaskState::Completed,
        CoordinationTaskState::Failed => WorkflowTaskState::Failed,
        CoordinationTaskState::Cancelled => WorkflowTaskState::Cancelled,
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/agent_run_workflow.rs"]
mod tests;
