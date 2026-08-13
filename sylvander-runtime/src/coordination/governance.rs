//! Deterministic graph limits and evidence-bearing moderator escalation.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sylvander_api::{AgentInstanceId, TaskId};

use crate::coordination::task::{CoordinationTaskState, SessionTaskGraph};
use crate::coordination::topology::{AgentRelationKind, SessionTopology};
use crate::session::membership::SessionMembership;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernancePolicy {
    pub max_agents: usize,
    pub max_ownership_depth: usize,
    pub max_children_per_agent: usize,
    pub max_active_tasks_per_agent: usize,
    pub max_message_delivery_attempts: u32,
    pub stagnation_window: usize,
    pub handoff_ping_pong_window: usize,
}

impl Default for GovernancePolicy {
    fn default() -> Self {
        Self {
            max_agents: 16,
            max_ownership_depth: 4,
            max_children_per_agent: 6,
            max_active_tasks_per_agent: 4,
            max_message_delivery_attempts: 5,
            stagnation_window: 3,
            handoff_ping_pong_window: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitDependency {
    pub task_id: TaskId,
    pub waiter: AgentInstanceId,
    pub awaited: AgentInstanceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressObservation {
    pub task_id: TaskId,
    pub agent_instance_id: AgentInstanceId,
    pub task_revision: u64,
    pub consumed_tokens: u64,
    pub evidence_digest: Option<String>,
    pub observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffObservation {
    pub task_id: TaskId,
    pub from: AgentInstanceId,
    pub to: AgentInstanceId,
    pub accepted_at: i64,
}

pub struct GovernanceSnapshot<'a> {
    pub membership: &'a SessionMembership,
    pub topology: &'a SessionTopology,
    pub tasks: &'a SessionTaskGraph,
    pub waits: &'a [WaitDependency],
    pub progress: &'a [ProgressObservation],
    pub handoffs: &'a [HandoffObservation],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    HardStop,
    ModeratorReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GovernanceFinding {
    AgentLimit {
        actual: usize,
        maximum: usize,
    },
    OwnershipDepth {
        actual: usize,
        maximum: usize,
    },
    OwnershipFanout {
        agent_instance_id: AgentInstanceId,
        actual: usize,
        maximum: usize,
    },
    TaskConcurrency {
        agent_instance_id: AgentInstanceId,
        actual: usize,
        maximum: usize,
    },
    TokenBudgetExhausted {
        task_id: TaskId,
    },
    WaitCycle {
        agents: Vec<AgentInstanceId>,
    },
    StagnantProgress {
        task_id: TaskId,
        observations: usize,
    },
    HandoffPingPong {
        task_id: TaskId,
        handoffs: usize,
    },
}

impl GovernanceFinding {
    #[must_use]
    pub const fn severity(&self) -> FindingSeverity {
        match self {
            Self::AgentLimit { .. }
            | Self::OwnershipDepth { .. }
            | Self::OwnershipFanout { .. }
            | Self::TaskConcurrency { .. }
            | Self::TokenBudgetExhausted { .. }
            | Self::WaitCycle { .. } => FindingSeverity::HardStop,
            Self::StagnantProgress { .. } | Self::HandoffPingPong { .. } => {
                FindingSeverity::ModeratorReview
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceAssessment {
    pub moderator_instance_id: AgentInstanceId,
    pub findings: Vec<GovernanceFinding>,
}

impl GovernanceAssessment {
    #[must_use]
    pub fn permits_automatic_progress(&self) -> bool {
        self.findings.is_empty()
    }

    #[must_use]
    pub fn has_hard_stop(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.severity() == FindingSeverity::HardStop)
    }
}

pub fn assess(
    policy: &GovernancePolicy,
    snapshot: &GovernanceSnapshot<'_>,
) -> GovernanceAssessment {
    let mut findings = Vec::new();
    if snapshot.membership.participants.len() > policy.max_agents {
        findings.push(GovernanceFinding::AgentLimit {
            actual: snapshot.membership.participants.len(),
            maximum: policy.max_agents,
        });
    }
    assess_topology(policy, snapshot, &mut findings);
    assess_tasks(policy, snapshot, &mut findings);
    for agents in wait_cycles(snapshot.waits) {
        findings.push(GovernanceFinding::WaitCycle { agents });
    }
    assess_progress(policy, snapshot.progress, &mut findings);
    assess_handoffs(policy, snapshot.handoffs, &mut findings);
    GovernanceAssessment {
        moderator_instance_id: snapshot.membership.governance.moderator_instance_id.clone(),
        findings,
    }
}

fn assess_topology(
    policy: &GovernancePolicy,
    snapshot: &GovernanceSnapshot<'_>,
    findings: &mut Vec<GovernanceFinding>,
) {
    let mut children: HashMap<AgentInstanceId, Vec<AgentInstanceId>> = HashMap::new();
    for relation in &snapshot.topology.relations {
        if relation.kind == AgentRelationKind::ParentOf {
            children
                .entry(relation.source.clone())
                .or_default()
                .push(relation.target.clone());
        }
    }
    for (agent, owned) in &children {
        if owned.len() > policy.max_children_per_agent {
            findings.push(GovernanceFinding::OwnershipFanout {
                agent_instance_id: agent.clone(),
                actual: owned.len(),
                maximum: policy.max_children_per_agent,
            });
        }
    }
    let mut maximum_depth = 0;
    let mut stack = vec![(
        snapshot.membership.governance.moderator_instance_id.clone(),
        0_usize,
    )];
    while let Some((agent, depth)) = stack.pop() {
        maximum_depth = maximum_depth.max(depth);
        stack.extend(
            children
                .get(&agent)
                .into_iter()
                .flatten()
                .cloned()
                .map(|child| (child, depth.saturating_add(1))),
        );
    }
    if maximum_depth > policy.max_ownership_depth {
        findings.push(GovernanceFinding::OwnershipDepth {
            actual: maximum_depth,
            maximum: policy.max_ownership_depth,
        });
    }
}

fn assess_tasks(
    policy: &GovernancePolicy,
    snapshot: &GovernanceSnapshot<'_>,
    findings: &mut Vec<GovernanceFinding>,
) {
    let mut active_by_agent: HashMap<&AgentInstanceId, usize> = HashMap::new();
    for task in &snapshot.tasks.tasks {
        if task.state.is_terminal() {
            continue;
        }
        if task.consumed_tokens >= task.token_budget {
            findings.push(GovernanceFinding::TokenBudgetExhausted {
                task_id: task.task_id.clone(),
            });
        }
        if matches!(
            task.state,
            CoordinationTaskState::Ready
                | CoordinationTaskState::Running
                | CoordinationTaskState::Blocked
                | CoordinationTaskState::AwaitingReview
        ) && let Some(assignee) = &task.assigned_to
        {
            *active_by_agent.entry(assignee).or_default() += 1;
        }
    }
    for (agent, actual) in active_by_agent {
        if actual > policy.max_active_tasks_per_agent {
            findings.push(GovernanceFinding::TaskConcurrency {
                agent_instance_id: agent.clone(),
                actual,
                maximum: policy.max_active_tasks_per_agent,
            });
        }
    }
}

fn assess_progress(
    policy: &GovernancePolicy,
    observations: &[ProgressObservation],
    findings: &mut Vec<GovernanceFinding>,
) {
    if policy.stagnation_window < 2 {
        return;
    }
    let mut by_task: HashMap<&TaskId, Vec<&ProgressObservation>> = HashMap::new();
    for observation in observations {
        by_task
            .entry(&observation.task_id)
            .or_default()
            .push(observation);
    }
    for (task_id, mut samples) in by_task {
        samples.sort_by_key(|sample| sample.observed_at);
        let window = samples
            .iter()
            .rev()
            .take(policy.stagnation_window)
            .copied()
            .collect::<Vec<_>>();
        if window.len() == policy.stagnation_window
            && window
                .iter()
                .all(|sample| sample.task_revision == window[0].task_revision)
            && window
                .iter()
                .all(|sample| sample.evidence_digest == window[0].evidence_digest)
            && window.iter().map(|sample| sample.consumed_tokens).max()
                > window.iter().map(|sample| sample.consumed_tokens).min()
        {
            findings.push(GovernanceFinding::StagnantProgress {
                task_id: task_id.clone(),
                observations: window.len(),
            });
        }
    }
}

fn assess_handoffs(
    policy: &GovernancePolicy,
    handoffs: &[HandoffObservation],
    findings: &mut Vec<GovernanceFinding>,
) {
    if policy.handoff_ping_pong_window < 2 {
        return;
    }
    let mut by_task: HashMap<&TaskId, Vec<&HandoffObservation>> = HashMap::new();
    for handoff in handoffs {
        by_task.entry(&handoff.task_id).or_default().push(handoff);
    }
    for (task_id, mut trace) in by_task {
        trace.sort_by_key(|handoff| handoff.accepted_at);
        let window = trace
            .iter()
            .rev()
            .take(policy.handoff_ping_pong_window)
            .copied()
            .collect::<Vec<_>>();
        if window.len() == policy.handoff_ping_pong_window
            && window
                .windows(2)
                .all(|pair| pair[0].from == pair[1].to && pair[0].to == pair[1].from)
        {
            findings.push(GovernanceFinding::HandoffPingPong {
                task_id: task_id.clone(),
                handoffs: window.len(),
            });
        }
    }
}

fn wait_cycles(waits: &[WaitDependency]) -> Vec<Vec<AgentInstanceId>> {
    let mut adjacency: HashMap<AgentInstanceId, Vec<AgentInstanceId>> = HashMap::new();
    for wait in waits {
        adjacency
            .entry(wait.waiter.clone())
            .or_default()
            .push(wait.awaited.clone());
        adjacency.entry(wait.awaited.clone()).or_default();
    }
    let mut cycles = Vec::new();
    let mut remaining: HashSet<_> = adjacency.keys().cloned().collect();
    while let Some(start) = remaining.iter().next().cloned() {
        let forward = reachable(&start, &adjacency);
        let reverse_graph = reverse(&adjacency);
        let backward = reachable(&start, &reverse_graph);
        let mut component = forward.intersection(&backward).cloned().collect::<Vec<_>>();
        for agent in &component {
            remaining.remove(agent);
        }
        let self_wait = adjacency
            .get(&start)
            .is_some_and(|neighbors| neighbors.contains(&start));
        if component.len() > 1 || self_wait {
            component.sort_by(|left, right| left.0.cmp(&right.0));
            cycles.push(component);
        }
    }
    cycles
}

fn reachable(
    start: &AgentInstanceId,
    adjacency: &HashMap<AgentInstanceId, Vec<AgentInstanceId>>,
) -> HashSet<AgentInstanceId> {
    let mut visited = HashSet::new();
    let mut stack = vec![start.clone()];
    while let Some(agent) = stack.pop() {
        if visited.insert(agent.clone()) {
            stack.extend(adjacency.get(&agent).into_iter().flatten().cloned());
        }
    }
    visited
}

fn reverse(
    adjacency: &HashMap<AgentInstanceId, Vec<AgentInstanceId>>,
) -> HashMap<AgentInstanceId, Vec<AgentInstanceId>> {
    let mut reversed: HashMap<_, Vec<_>> = adjacency
        .keys()
        .cloned()
        .map(|agent| (agent, Vec::new()))
        .collect();
    for (source, targets) in adjacency {
        for target in targets {
            reversed
                .entry(target.clone())
                .or_default()
                .push(source.clone());
        }
    }
    reversed
}

#[cfg(test)]
#[path = "../../tests/unit/coordination_governance.rs"]
mod tests;
