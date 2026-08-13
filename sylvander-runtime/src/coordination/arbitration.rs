//! Fenced, AI-native moderator arbitration for coordination findings.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sylvander_api::{AgentInstanceId, GovernanceCaseId, SessionId, TaskId};

use crate::coordination::governance::{FindingSeverity, GovernanceFinding};
use crate::coordination::task::SessionTaskGraph;
use crate::session::membership::SessionMembership;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArbitrationState {
    Open,
    Decided,
    Applying,
    Applied,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArbitrationCase {
    pub case_id: GovernanceCaseId,
    pub session_id: SessionId,
    pub moderator_instance_id: AgentInstanceId,
    pub membership_revision: u64,
    pub topology_revision: u64,
    pub moderator_lease_epoch: u64,
    pub moderator_fencing_token: u64,
    pub findings: Vec<GovernanceFinding>,
    pub state: ArbitrationState,
    pub revision: u64,
    pub expires_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ArbitrationCase {
    pub fn validate_new(
        &self,
        membership: &SessionMembership,
        topology_revision: u64,
        now: i64,
    ) -> Result<(), ArbitrationError> {
        if self.state != ArbitrationState::Open || self.revision != 0 {
            return Err(ArbitrationError::NotNew);
        }
        if self.findings.is_empty() {
            return Err(ArbitrationError::NoFindings);
        }
        if self.session_id != membership.session_id
            || self.moderator_instance_id != membership.governance.moderator_instance_id
            || self.membership_revision != membership.governance.membership_revision
            || self.topology_revision != topology_revision
            || self.moderator_lease_epoch != membership.governance.lease_epoch
            || self.moderator_fencing_token != membership.governance.fencing_token
        {
            return Err(ArbitrationError::StaleGovernance);
        }
        if self.expires_at <= now {
            return Err(ArbitrationError::Expired);
        }
        Ok(())
    }

    #[must_use]
    pub fn has_hard_stop(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.severity() == FindingSeverity::HardStop)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum ModeratorVerdict {
    ContinueWithConditions {
        conditions: Vec<String>,
    },
    Replan {
        task_ids: Vec<TaskId>,
    },
    Reassign {
        task_id: TaskId,
        to_instance_id: AgentInstanceId,
    },
    SuspendAgents {
        agent_instance_ids: Vec<AgentInstanceId>,
    },
    CancelTasks {
        task_ids: Vec<TaskId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeratorDecision {
    pub case_id: GovernanceCaseId,
    pub decided_by: AgentInstanceId,
    pub moderator_lease_epoch: u64,
    pub moderator_fencing_token: u64,
    pub verdict: ModeratorVerdict,
    pub rationale: String,
    pub evidence_refs: Vec<String>,
    pub decided_at: i64,
}

impl ModeratorDecision {
    pub fn validate(
        &self,
        case: &ArbitrationCase,
        membership: &SessionMembership,
        tasks: &SessionTaskGraph,
        topology_revision: u64,
        now: i64,
    ) -> Result<(), ArbitrationError> {
        if case.state != ArbitrationState::Open || case.expires_at <= now {
            return Err(ArbitrationError::Expired);
        }
        if self.case_id != case.case_id
            || self.decided_by != case.moderator_instance_id
            || self.decided_by != membership.governance.moderator_instance_id
        {
            return Err(ArbitrationError::UnauthorizedModerator);
        }
        if self.moderator_lease_epoch != case.moderator_lease_epoch
            || self.moderator_fencing_token != case.moderator_fencing_token
            || self.moderator_lease_epoch != membership.governance.lease_epoch
            || self.moderator_fencing_token != membership.governance.fencing_token
            || case.membership_revision != membership.governance.membership_revision
            || case.topology_revision != topology_revision
            || tasks.membership_revision != case.membership_revision
            || tasks.session_id != case.session_id
        {
            return Err(ArbitrationError::StaleGovernance);
        }
        if self.rationale.trim().is_empty()
            || self.evidence_refs.iter().any(|item| item.trim().is_empty())
        {
            return Err(ArbitrationError::MissingReasoning);
        }
        if case.has_hard_stop()
            && matches!(
                self.verdict,
                ModeratorVerdict::ContinueWithConditions { .. }
            )
        {
            return Err(ArbitrationError::HardStopCannotContinue);
        }
        validate_verdict(&self.verdict, membership, tasks)
    }
}

fn validate_verdict(
    verdict: &ModeratorVerdict,
    membership: &SessionMembership,
    tasks: &SessionTaskGraph,
) -> Result<(), ArbitrationError> {
    let members: HashSet<_> = membership
        .participants
        .iter()
        .map(|participant| &participant.instance_id)
        .collect();
    let task_ids: HashSet<_> = tasks.tasks.iter().map(|task| &task.task_id).collect();
    match verdict {
        ModeratorVerdict::ContinueWithConditions { conditions } => {
            if conditions.is_empty() || conditions.iter().any(|item| item.trim().is_empty()) {
                return Err(ArbitrationError::EmptyRemediation);
            }
        }
        ModeratorVerdict::Replan { task_ids: selected }
        | ModeratorVerdict::CancelTasks { task_ids: selected } => {
            if selected.is_empty() || selected.iter().any(|task| !task_ids.contains(task)) {
                return Err(ArbitrationError::UnknownTask);
            }
        }
        ModeratorVerdict::Reassign {
            task_id,
            to_instance_id,
        } => {
            if !task_ids.contains(task_id) {
                return Err(ArbitrationError::UnknownTask);
            }
            if !members.contains(to_instance_id) {
                return Err(ArbitrationError::UnknownAgent);
            }
        }
        ModeratorVerdict::SuspendAgents { agent_instance_ids } => {
            if agent_instance_ids.is_empty()
                || agent_instance_ids
                    .iter()
                    .any(|agent| !members.contains(agent))
                || agent_instance_ids
                    .iter()
                    .any(|agent| agent == &membership.governance.moderator_instance_id)
            {
                return Err(ArbitrationError::UnknownAgent);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArbitrationError {
    #[error("arbitration case is not new")]
    NotNew,
    #[error("arbitration case contains no governance findings")]
    NoFindings,
    #[error("arbitration facts do not match current Session governance")]
    StaleGovernance,
    #[error("arbitration case is expired or no longer open")]
    Expired,
    #[error("decision was not issued by the fenced Session moderator")]
    UnauthorizedModerator,
    #[error("moderator decision requires rationale and valid evidence references")]
    MissingReasoning,
    #[error("a hard-stop finding cannot be overridden with continuation")]
    HardStopCannotContinue,
    #[error("moderator verdict contains no actionable remediation")]
    EmptyRemediation,
    #[error("moderator verdict references an unknown task")]
    UnknownTask,
    #[error("moderator verdict references an unknown or protected Agent")]
    UnknownAgent,
}

#[cfg(test)]
#[path = "../../tests/unit/coordination_arbitration.rs"]
mod tests;
