use serde::{Deserialize, Serialize};
use std::sync::Arc;
use sylvander_agent::doctor_gate::{DoctorAttention, DoctorGate, DoctorReport};
use sylvander_api::{AgentInstanceId, SessionId};

use super::{Runtime, RuntimeError};
use crate::agent::instance::AgentInstanceState;
use crate::agent::run::AuthenticatedSession;
use crate::coordination::task::CoordinationTaskState;
use crate::coordination::workspace::WorkspaceViewState;
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::{SessionStore, SqliteSessionStore};
use crate::storage::workspace_coordination::AgentWorkspaceStore;

pub(crate) struct RuntimeDoctorGate {
    pub(crate) store: Arc<SqliteSessionStore>,
    pub(crate) session_id: SessionId,
    pub(crate) agent_instance_id: AgentInstanceId,
}

#[async_trait::async_trait]
impl DoctorGate for RuntimeDoctorGate {
    async fn inspect(&self) -> Result<DoctorReport, String> {
        let membership = self
            .store
            .session_membership(&self.session_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Agent membership does not exist".to_owned())?;
        if !membership
            .participants
            .iter()
            .any(|participant| participant.instance_id == self.agent_instance_id)
        {
            return Err("Agent is not a member of this Session".into());
        }
        let topology = self
            .store
            .topology(&self.session_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Session topology does not exist".to_owned())?;
        let tasks = self
            .store
            .task_graph(&self.session_id)
            .await
            .map_err(|error| error.to_string())?;
        let arbitrations = self
            .store
            .active_arbitration_cases(&self.session_id)
            .await
            .map_err(|error| error.to_string())?;
        let workspaces = self
            .store
            .active_workspace_views(&self.session_id)
            .await
            .map_err(|error| error.to_string())?;
        let models = self
            .store
            .interrupted_model_iterations()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|iteration| iteration.session_id == self.session_id)
            .collect::<Vec<_>>();
        let tools = self
            .store
            .interrupted_tool_calls()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|call| call.session_id == self.session_id)
            .collect::<Vec<_>>();
        let agents = summarize_agents(&membership.participants);
        let tasks = summarize_tasks(tasks.as_ref());
        let workspaces = summarize_workspaces(&workspaces);
        let recovery = SessionRecoverySummary {
            interrupted_models: models.len() as u64,
            interrupted_tools: tools.len() as u64,
            operator_models: models
                .iter()
                .filter(|iteration| iteration.operator_action_required)
                .count() as u64,
            operator_tools: tools
                .iter()
                .filter(|call| call.operator_action_required)
                .count() as u64,
        };
        let governance = SessionGovernanceSummary {
            topology_relations: topology.relations.len() as u64,
            open_arbitrations: arbitrations.len() as u64,
        };
        let attention = attention(&agents, &tasks, &workspaces, &recovery, &governance);
        Ok(DoctorReport {
            attention: doctor_attention(attention),
            active_agents: agents.active,
            waiting_agents: agents.waiting,
            manual_agents: agents.manual_reconciliation,
            ready_tasks: tasks.ready,
            running_tasks: tasks.running,
            blocked_tasks: tasks.blocked,
            review_tasks: tasks.awaiting_review,
            remaining_token_budget: tasks.remaining_token_budget,
            integrating_workspaces: workspaces.integrating,
            conflicted_workspaces: workspaces.conflicted,
            manual_workspaces: workspaces.manual_reconciliation,
            interrupted_models: recovery.interrupted_models,
            interrupted_tools: recovery.interrupted_tools,
            operator_recoveries: recovery.operator_models + recovery.operator_tools,
            open_arbitrations: governance.open_arbitrations,
        })
    }
}

const fn doctor_attention(attention: SessionAttentionState) -> DoctorAttention {
    match attention {
        SessionAttentionState::Healthy => DoctorAttention::Healthy,
        SessionAttentionState::Active => DoctorAttention::Active,
        SessionAttentionState::Waiting => DoctorAttention::Waiting,
        SessionAttentionState::Recovering => DoctorAttention::Recovering,
        SessionAttentionState::NeedsReview => DoctorAttention::NeedsReview,
        SessionAttentionState::ManualActionRequired => DoctorAttention::ManualActionRequired,
    }
}

/// Highest-priority durable condition currently visible to an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAttentionState {
    Healthy,
    Active,
    Waiting,
    Recovering,
    NeedsReview,
    ManualActionRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAgentSummary {
    pub total: u64,
    pub active: u64,
    pub waiting: u64,
    pub terminal: u64,
    pub manual_reconciliation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTaskSummary {
    pub total: u64,
    pub ready: u64,
    pub running: u64,
    pub blocked: u64,
    pub awaiting_review: u64,
    pub terminal: u64,
    pub remaining_token_budget: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWorkspaceSummary {
    pub active: u64,
    pub integrating: u64,
    pub conflicted: u64,
    pub manual_reconciliation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecoverySummary {
    pub interrupted_models: u64,
    pub interrupted_tools: u64,
    pub operator_models: u64,
    pub operator_tools: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGovernanceSummary {
    pub topology_relations: u64,
    pub open_arbitrations: u64,
}

/// Content-safe Doctor view derived entirely from authoritative durable facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDoctorProjection {
    pub session_id: SessionId,
    pub moderator_instance_id: AgentInstanceId,
    pub membership_revision: u64,
    pub topology_revision: u64,
    pub attention: SessionAttentionState,
    pub agents: SessionAgentSummary,
    pub tasks: SessionTaskSummary,
    pub workspaces: SessionWorkspaceSummary,
    pub recovery: SessionRecoverySummary,
    pub governance: SessionGovernanceSummary,
}

impl Runtime {
    /// Reconstruct one Session's operational truth after validating membership.
    pub async fn session_doctor(
        &self,
        actor: &AuthenticatedSession,
    ) -> Result<SessionDoctorProjection, RuntimeError> {
        let session_id = actor.id();
        let membership = self
            .storage
            .sessions()
            .session_membership(session_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .ok_or_else(|| RuntimeError::Coordination("Agent membership does not exist".into()))?;
        if !membership
            .participants
            .iter()
            .any(|participant| &participant.instance_id == actor.agent_instance_id())
        {
            return Err(RuntimeError::Coordination(
                "Agent is not a member of this Session".into(),
            ));
        }
        let coordination = self.coordination_service()?;
        let topology = coordination
            .topology(session_id)
            .await
            .map_err(super::coordination_error)?;
        let graph = coordination
            .task_graph(session_id)
            .await
            .map_err(super::coordination_error)?;
        let arbitrations = coordination
            .active_arbitration_cases(session_id)
            .await
            .map_err(super::coordination_error)?;
        let views = match &self.agent_workspaces {
            Some(workspaces) => workspaces
                .active_views(session_id)
                .await
                .map_err(|error| RuntimeError::Coordination(error.to_string()))?,
            None => Vec::new(),
        };
        let models = self
            .storage
            .sessions()
            .interrupted_model_iterations()
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .into_iter()
            .filter(|iteration| &iteration.session_id == session_id)
            .collect::<Vec<_>>();
        let tools = self
            .storage
            .sessions()
            .interrupted_tool_calls()
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
            .into_iter()
            .filter(|call| &call.session_id == session_id)
            .collect::<Vec<_>>();

        let agents = summarize_agents(&membership.participants);
        let tasks = summarize_tasks(graph.as_ref());
        let workspaces = summarize_workspaces(&views);
        let recovery = SessionRecoverySummary {
            interrupted_models: models.len() as u64,
            interrupted_tools: tools.len() as u64,
            operator_models: models
                .iter()
                .filter(|iteration| iteration.operator_action_required)
                .count() as u64,
            operator_tools: tools
                .iter()
                .filter(|call| call.operator_action_required)
                .count() as u64,
        };
        let governance = SessionGovernanceSummary {
            topology_relations: topology.relations.len() as u64,
            open_arbitrations: arbitrations.len() as u64,
        };
        let attention = attention(&agents, &tasks, &workspaces, &recovery, &governance);
        Ok(SessionDoctorProjection {
            session_id: session_id.clone(),
            moderator_instance_id: membership.governance.moderator_instance_id,
            membership_revision: membership.governance.membership_revision,
            topology_revision: topology.topology_revision,
            attention,
            agents,
            tasks,
            workspaces,
            recovery,
            governance,
        })
    }
}

fn summarize_agents(participants: &[crate::agent::instance::AgentInstance]) -> SessionAgentSummary {
    SessionAgentSummary {
        total: participants.len() as u64,
        active: participants
            .iter()
            .filter(|agent| {
                matches!(
                    agent.state,
                    AgentInstanceState::Ready | AgentInstanceState::Running
                )
            })
            .count() as u64,
        waiting: participants
            .iter()
            .filter(|agent| {
                matches!(
                    agent.state,
                    AgentInstanceState::WaitingMessage | AgentInstanceState::WaitingApproval
                )
            })
            .count() as u64,
        terminal: participants
            .iter()
            .filter(|agent| agent.state.is_terminal())
            .count() as u64,
        manual_reconciliation: participants
            .iter()
            .filter(|agent| agent.state == AgentInstanceState::ManualReconciliation)
            .count() as u64,
    }
}

fn summarize_tasks(
    graph: Option<&crate::coordination::task::SessionTaskGraph>,
) -> SessionTaskSummary {
    let tasks = graph.map_or(&[][..], |graph| graph.tasks.as_slice());
    let count = |state| tasks.iter().filter(|task| task.state == state).count() as u64;
    SessionTaskSummary {
        total: tasks.len() as u64,
        ready: count(CoordinationTaskState::Ready),
        running: count(CoordinationTaskState::Running),
        blocked: count(CoordinationTaskState::Blocked),
        awaiting_review: count(CoordinationTaskState::AwaitingReview),
        terminal: tasks.iter().filter(|task| task.state.is_terminal()).count() as u64,
        remaining_token_budget: tasks
            .iter()
            .filter(|task| !task.state.is_terminal())
            .map(|task| task.token_budget.saturating_sub(task.consumed_tokens))
            .sum(),
    }
}

fn summarize_workspaces(
    views: &[crate::coordination::workspace::AgentWorkspaceView],
) -> SessionWorkspaceSummary {
    let count = |state| views.iter().filter(|view| view.state == state).count() as u64;
    SessionWorkspaceSummary {
        active: count(WorkspaceViewState::Active),
        integrating: count(WorkspaceViewState::Integrating),
        conflicted: count(WorkspaceViewState::Conflicted),
        manual_reconciliation: count(WorkspaceViewState::ManualReconciliation),
    }
}

fn attention(
    agents: &SessionAgentSummary,
    tasks: &SessionTaskSummary,
    workspaces: &SessionWorkspaceSummary,
    recovery: &SessionRecoverySummary,
    governance: &SessionGovernanceSummary,
) -> SessionAttentionState {
    if agents.manual_reconciliation
        + workspaces.manual_reconciliation
        + recovery.operator_models
        + recovery.operator_tools
        > 0
    {
        SessionAttentionState::ManualActionRequired
    } else if governance.open_arbitrations + tasks.awaiting_review + workspaces.conflicted > 0 {
        SessionAttentionState::NeedsReview
    } else if recovery.interrupted_models + recovery.interrupted_tools > 0 {
        SessionAttentionState::Recovering
    } else if tasks.blocked + agents.waiting > 0 {
        SessionAttentionState::Waiting
    } else if tasks.running + tasks.ready + agents.active + workspaces.integrating > 0 {
        SessionAttentionState::Active
    } else {
        SessionAttentionState::Healthy
    }
}
