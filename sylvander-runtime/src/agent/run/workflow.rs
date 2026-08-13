//! Runtime implementation of Agent-authored durable workflow intents.

use std::sync::Arc;

use sylvander_agent::workflow_gate::{
    WorkflowCommand, WorkflowGate, WorkflowReceipt, WorkflowTaskState,
};
use sylvander_api::{AgentInstanceId, SessionId, TaskId};

use crate::coordination::governance::GovernancePolicy;
use crate::coordination::service::{
    CoordinationService, CreateTaskRequest, DEFAULT_ARBITRATION_TTL_SECONDS, TransitionTaskRequest,
};
use crate::coordination::task::CoordinationTaskState;
use crate::session::now_secs;
use crate::storage::session::SqliteSessionStore;

pub(super) struct RuntimeWorkflowGate {
    pub(super) store: Arc<SqliteSessionStore>,
    pub(super) session_id: SessionId,
    pub(super) agent_instance_id: AgentInstanceId,
}

#[async_trait::async_trait]
impl WorkflowGate for RuntimeWorkflowGate {
    async fn apply(&self, command: WorkflowCommand) -> Result<WorkflowReceipt, String> {
        let service = CoordinationService::new(
            self.store.clone(),
            GovernancePolicy::default(),
            DEFAULT_ARBITRATION_TTL_SECONDS,
        );
        let task = match command {
            WorkflowCommand::Create {
                task_id,
                objective,
                token_budget,
                max_handoffs,
            } => {
                service
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
            }
            WorkflowCommand::Transition {
                task_id,
                state,
                consumed_tokens,
            } => {
                service
                    .transition_task(
                        TransitionTaskRequest {
                            task_id: TaskId::new(task_id),
                            session_id: self.session_id.clone(),
                            actor: self.agent_instance_id.clone(),
                            next_state: runtime_state(state),
                            consumed_tokens,
                        },
                        now_secs(),
                    )
                    .await
            }
        }
        .map_err(|error| error.to_string())?;
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
mod tests {
    use super::*;

    #[test]
    fn every_runtime_state_has_an_agent_visible_projection() {
        assert_eq!(
            agent_state(CoordinationTaskState::Proposed),
            WorkflowTaskState::Ready
        );
        assert_eq!(
            runtime_state(WorkflowTaskState::AwaitingReview),
            CoordinationTaskState::AwaitingReview
        );
        assert_eq!(
            runtime_state(WorkflowTaskState::Cancelled),
            CoordinationTaskState::Cancelled
        );
    }
}
