//! Durable work graph and hard progress invariants for multi-Agent execution.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use sylvander_api::{AgentInstanceId, SessionId, TaskId};

use crate::session::membership::SessionMembership;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinationTaskState {
    Proposed,
    Ready,
    Running,
    Blocked,
    AwaitingReview,
    Completed,
    Failed,
    Cancelled,
}

impl CoordinationTaskState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self.is_terminal() || self == next {
            return false;
        }
        matches!(
            (self, next),
            (Self::Proposed, Self::Ready | Self::Cancelled)
                | (Self::Ready, Self::Running | Self::Cancelled)
                | (
                    Self::Running,
                    Self::Blocked
                        | Self::AwaitingReview
                        | Self::Completed
                        | Self::Failed
                        | Self::Cancelled
                )
                | (Self::Blocked, Self::Ready | Self::Failed | Self::Cancelled)
                | (
                    Self::AwaitingReview,
                    Self::Running | Self::Completed | Self::Failed | Self::Cancelled
                )
        )
    }
}

/// One recoverable unit of work with explicit resource and handoff ceilings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinationTask {
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub membership_revision: u64,
    pub parent_task_id: Option<TaskId>,
    pub created_by: AgentInstanceId,
    pub assigned_to: Option<AgentInstanceId>,
    pub objective: String,
    pub state: CoordinationTaskState,
    pub token_budget: u64,
    pub consumed_tokens: u64,
    pub max_handoffs: u32,
    pub handoff_count: u32,
    pub revision: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Runtime-issued ownership of one running task. The opaque token and
/// monotonic epoch fence late commits from an executor replaced after crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskExecutionLease {
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub assignee: AgentInstanceId,
    pub task_revision: u64,
    pub lease_epoch: u64,
    pub fencing_token: String,
    pub expires_at: i64,
}

pub const MAX_TASK_LEASE_SECONDS: u64 = 300;

/// `prerequisite` must become terminal-success before `dependent` may run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskDependency {
    pub prerequisite: TaskId,
    pub dependent: TaskId,
}

/// Exact task graph synchronized to one membership revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTaskGraph {
    pub session_id: SessionId,
    pub membership_revision: u64,
    pub tasks: Vec<CoordinationTask>,
    pub dependencies: Vec<TaskDependency>,
}

impl SessionTaskGraph {
    pub fn validate(&self, membership: &SessionMembership) -> Result<(), TaskGraphError> {
        if self.session_id != membership.session_id
            || self.membership_revision != membership.governance.membership_revision
        {
            return Err(TaskGraphError::MembershipMismatch);
        }
        let members: HashSet<_> = membership
            .participants
            .iter()
            .map(|participant| participant.instance_id.clone())
            .collect();
        let mut tasks = HashMap::with_capacity(self.tasks.len());
        for task in &self.tasks {
            if task.session_id != self.session_id
                || task.membership_revision != self.membership_revision
            {
                return Err(TaskGraphError::TaskSessionMismatch(task.task_id.clone()));
            }
            if task.objective.trim().is_empty() || task.token_budget == 0 {
                return Err(TaskGraphError::InvalidTask(task.task_id.clone()));
            }
            if task.consumed_tokens > task.token_budget || task.handoff_count > task.max_handoffs {
                return Err(TaskGraphError::BudgetExceeded(task.task_id.clone()));
            }
            if !members.contains(&task.created_by)
                || task
                    .assigned_to
                    .as_ref()
                    .is_some_and(|assignee| !members.contains(assignee))
            {
                return Err(TaskGraphError::UnknownActor(task.task_id.clone()));
            }
            if tasks.insert(task.task_id.clone(), task).is_some() {
                return Err(TaskGraphError::DuplicateTask(task.task_id.clone()));
            }
        }

        let parent_edges = self
            .tasks
            .iter()
            .filter_map(|task| {
                task.parent_task_id
                    .as_ref()
                    .map(|parent| (parent.clone(), task.task_id.clone()))
            })
            .collect::<Vec<_>>();
        validate_acyclic_edges(&tasks, &parent_edges, TaskGraphError::ParentCycle)?;

        let mut unique_dependencies = HashSet::with_capacity(self.dependencies.len());
        let dependency_edges = self
            .dependencies
            .iter()
            .map(|dependency| {
                if !unique_dependencies.insert(dependency.clone()) {
                    return Err(TaskGraphError::DuplicateDependency);
                }
                Ok((
                    dependency.prerequisite.clone(),
                    dependency.dependent.clone(),
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        validate_acyclic_edges(&tasks, &dependency_edges, TaskGraphError::DependencyCycle)
    }
}

fn validate_acyclic_edges(
    tasks: &HashMap<TaskId, &CoordinationTask>,
    edges: &[(TaskId, TaskId)],
    cycle_error: TaskGraphError,
) -> Result<(), TaskGraphError> {
    let mut indegree: HashMap<_, usize> = tasks.keys().cloned().map(|id| (id, 0)).collect();
    let mut successors: HashMap<TaskId, Vec<TaskId>> = HashMap::new();
    for (source, target) in edges {
        if source == target {
            return Err(cycle_error);
        }
        let Some(target_degree) = indegree.get_mut(target) else {
            return Err(TaskGraphError::UnknownTask(target.clone()));
        };
        if !tasks.contains_key(source) {
            return Err(TaskGraphError::UnknownTask(source.clone()));
        }
        *target_degree = target_degree.saturating_add(1);
        successors
            .entry(source.clone())
            .or_default()
            .push(target.clone());
    }
    let mut queue: VecDeque<_> = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(id.clone()))
        .collect();
    let mut visited = 0_usize;
    while let Some(task_id) = queue.pop_front() {
        visited = visited.saturating_add(1);
        for successor in successors.get(&task_id).into_iter().flatten() {
            let degree = indegree
                .get_mut(successor)
                .expect("validated successor exists");
            *degree -= 1;
            if *degree == 0 {
                queue.push_back(successor.clone());
            }
        }
    }
    if visited == tasks.len() {
        Ok(())
    } else {
        Err(cycle_error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskGraphError {
    #[error("task graph is not synchronized to current Session membership")]
    MembershipMismatch,
    #[error("task {0} belongs to a different Session")]
    TaskSessionMismatch(TaskId),
    #[error("task {0} has an empty objective or zero token budget")]
    InvalidTask(TaskId),
    #[error("task {0} exceeded its token or handoff budget")]
    BudgetExceeded(TaskId),
    #[error("task {0} references an unknown Agent instance")]
    UnknownActor(TaskId),
    #[error("task {0} appears more than once")]
    DuplicateTask(TaskId),
    #[error("task graph references unknown task {0}")]
    UnknownTask(TaskId),
    #[error("task parent hierarchy contains a cycle")]
    ParentCycle,
    #[error("task dependency appears more than once")]
    DuplicateDependency,
    #[error("task dependency graph contains a cycle")]
    DependencyCycle,
}

#[cfg(test)]
#[path = "../../tests/unit/coordination_task.rs"]
mod tests;
