//! Agent-driven, governed lifecycle for durable work graph nodes.

use super::{
    CancelTaskRequest, ClaimTaskRequest, CoordinationService, CoordinationServiceError,
    CoordinationTask, CoordinationTaskState, CreateTaskRequest, FinishClaimedTaskRequest,
    GovernanceSnapshot, SessionTaskGraph, TransitionTaskRequest, assess, ensure_available,
};
use crate::coordination::governance::FindingSeverity;
use crate::coordination::topology::AgentRelationKind;
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;

impl<S> CoordinationService<S>
where
    S: AgentInstanceStore + CoordinationStore,
{
    /// Validate and persist a bounded task authored by an active Agent.
    pub async fn create_task(
        &self,
        request: CreateTaskRequest,
        now: i64,
    ) -> Result<CoordinationTask, CoordinationServiceError> {
        if request.objective.trim().is_empty() || request.token_budget == 0 {
            return Err(CoordinationServiceError::InvalidTask(
                "objective and token budget are required".into(),
            ));
        }
        let membership = self
            .store
            .session_membership(&request.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.session_id.clone())
            })?;
        membership
            .validate()
            .map_err(|error| CoordinationServiceError::InvalidDurableFacts(error.to_string()))?;
        ensure_available(&membership, &request.created_by)?;
        ensure_available(&membership, &request.assigned_to)?;
        let topology = self
            .store
            .topology(&request.session_id)
            .await?
            .ok_or_else(|| CoordinationServiceError::MissingTopology(request.session_id.clone()))?;
        topology
            .validate(&membership)
            .map_err(|error| CoordinationServiceError::InvalidDurableFacts(error.to_string()))?;
        if request.created_by != request.assigned_to
            && request.created_by != membership.governance.moderator_instance_id
            && !topology.relations.iter().any(|relation| {
                relation.kind == AgentRelationKind::ParentOf
                    && relation.source == request.created_by
                    && relation.target == request.assigned_to
            })
        {
            return Err(CoordinationServiceError::UnauthorizedActor);
        }

        if let Some(existing) = self.store.task(&request.task_id).await? {
            return if same_task_intent(&existing, &request) {
                Ok(existing)
            } else {
                Err(CoordinationServiceError::IdempotencyConflict)
            };
        }

        let mut graph = self
            .store
            .task_graph(&request.session_id)
            .await?
            .unwrap_or_else(|| SessionTaskGraph {
                session_id: request.session_id.clone(),
                membership_revision: membership.governance.membership_revision,
                tasks: Vec::new(),
                dependencies: Vec::new(),
            });
        if request
            .parent_task_id
            .as_ref()
            .is_some_and(|parent| !graph.tasks.iter().any(|task| &task.task_id == parent))
        {
            return Err(CoordinationServiceError::UnknownTask);
        }
        let task = CoordinationTask {
            task_id: request.task_id,
            session_id: request.session_id,
            membership_revision: membership.governance.membership_revision,
            parent_task_id: request.parent_task_id,
            created_by: request.created_by,
            assigned_to: Some(request.assigned_to),
            objective: request.objective,
            state: CoordinationTaskState::Ready,
            token_budget: request.token_budget,
            consumed_tokens: 0,
            max_handoffs: request.max_handoffs,
            handoff_count: 0,
            revision: 0,
            created_at: now,
            updated_at: now,
        };
        graph.tasks.push(task.clone());
        graph
            .validate(&membership)
            .map_err(|error| CoordinationServiceError::InvalidTask(error.to_string()))?;
        let observations = self
            .store
            .governance_observations(&graph.session_id, 1)
            .await?;
        let assessment = assess(
            &self.policy,
            &GovernanceSnapshot {
                membership: &membership,
                topology: &topology,
                tasks: &graph,
                waits: &observations.waits,
                progress: &observations.progress,
                handoffs: &observations.handoffs,
            },
        );
        if assessment
            .findings
            .iter()
            .any(|finding| finding.severity() == FindingSeverity::HardStop)
        {
            return Err(CoordinationServiceError::GovernanceBlocked);
        }
        self.store.create_task(&task).await?;
        Ok(task)
    }

    /// Advance a task under optimistic revision fencing. Ordinary Agents own
    /// their execution states; only the moderator may accept reviewed work.
    pub async fn transition_task(
        &self,
        request: TransitionTaskRequest,
        now: i64,
    ) -> Result<CoordinationTask, CoordinationServiceError> {
        let membership = self
            .store
            .session_membership(&request.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.session_id.clone())
            })?;
        ensure_available(&membership, &request.actor)?;
        let mut task = self
            .store
            .task(&request.task_id)
            .await?
            .ok_or(CoordinationServiceError::UnknownTask)?;
        if task.session_id != request.session_id {
            return Err(CoordinationServiceError::UnknownTask);
        }
        let moderator = request.actor == membership.governance.moderator_instance_id;
        let assignee = task.assigned_to.as_ref() == Some(&request.actor);
        let authorized = if task.state == CoordinationTaskState::AwaitingReview
            && request.next_state == CoordinationTaskState::Completed
        {
            moderator
        } else if request.next_state == CoordinationTaskState::Cancelled {
            moderator || assignee || task.created_by == request.actor
        } else {
            assignee
        };
        if task.state == CoordinationTaskState::Running
            || request.next_state == CoordinationTaskState::Running
        {
            return Err(CoordinationServiceError::InvalidTask(
                "running task transitions require an execution lease".into(),
            ));
        }
        if !authorized {
            return Err(CoordinationServiceError::UnauthorizedActor);
        }
        if request.consumed_tokens < task.consumed_tokens
            || request.consumed_tokens > task.token_budget
        {
            return Err(CoordinationServiceError::InvalidTask(
                "consumed token total is outside the task budget".into(),
            ));
        }
        let expected_revision = task.revision;
        task.state = request.next_state;
        task.consumed_tokens = request.consumed_tokens;
        task.revision = task
            .revision
            .checked_add(1)
            .ok_or(CoordinationServiceError::InvalidConfiguration)?;
        task.updated_at = now;
        self.store.update_task(&task, expected_revision).await?;
        Ok(task)
    }

    /// Atomically enter or recover task execution under a bounded lease.
    pub async fn claim_task(
        &self,
        request: ClaimTaskRequest,
        now: i64,
    ) -> Result<crate::coordination::task::TaskExecutionLease, CoordinationServiceError> {
        let membership = self
            .store
            .session_membership(&request.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.session_id.clone())
            })?;
        ensure_available(&membership, &request.actor)?;
        let task = self
            .store
            .task(&request.task_id)
            .await?
            .ok_or(CoordinationServiceError::UnknownTask)?;
        if task.session_id != request.session_id
            || task.assigned_to.as_ref() != Some(&request.actor)
        {
            return Err(CoordinationServiceError::UnauthorizedActor);
        }
        self.store
            .claim_task(
                &request.task_id,
                &request.actor,
                &request.claim_owner_id,
                now,
                request.lease_seconds,
            )
            .await
            .map_err(Into::into)
    }

    /// Commit a running task boundary only while the exact execution lease is live.
    pub async fn finish_claimed_task(
        &self,
        request: FinishClaimedTaskRequest,
        now: i64,
    ) -> Result<CoordinationTask, CoordinationServiceError> {
        let membership = self
            .store
            .session_membership(&request.lease.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.lease.session_id.clone())
            })?;
        ensure_available(&membership, &request.lease.assignee)?;
        if !matches!(
            request.next_state,
            CoordinationTaskState::Blocked
                | CoordinationTaskState::AwaitingReview
                | CoordinationTaskState::Completed
                | CoordinationTaskState::Failed
                | CoordinationTaskState::Cancelled
        ) {
            return Err(CoordinationServiceError::InvalidTask(
                "claimed task target state is not an execution boundary".into(),
            ));
        }
        self.store
            .finish_task_lease(
                &request.lease,
                request.next_state,
                request.consumed_tokens,
                now,
            )
            .await
            .map_err(Into::into)
    }

    /// Fence any active executor and durably cancel unfinished work.
    pub async fn cancel_task(
        &self,
        request: CancelTaskRequest,
        now: i64,
    ) -> Result<CoordinationTask, CoordinationServiceError> {
        let membership = self
            .store
            .session_membership(&request.session_id)
            .await?
            .ok_or_else(|| {
                CoordinationServiceError::MissingMembership(request.session_id.clone())
            })?;
        ensure_available(&membership, &request.actor)?;
        let task = self
            .store
            .task(&request.task_id)
            .await?
            .ok_or(CoordinationServiceError::UnknownTask)?;
        if task.session_id != request.session_id
            || (request.actor != membership.governance.moderator_instance_id
                && task.assigned_to.as_ref() != Some(&request.actor)
                && task.created_by != request.actor)
        {
            return Err(CoordinationServiceError::UnauthorizedActor);
        }
        self.store
            .cancel_task(&request.task_id, now)
            .await
            .map_err(Into::into)
    }
}

fn same_task_intent(task: &CoordinationTask, request: &CreateTaskRequest) -> bool {
    task.task_id == request.task_id
        && task.session_id == request.session_id
        && task.parent_task_id == request.parent_task_id
        && task.created_by == request.created_by
        && task.assigned_to.as_ref() == Some(&request.assigned_to)
        && task.objective == request.objective
        && task.token_budget == request.token_budget
        && task.max_handoffs == request.max_handoffs
}
