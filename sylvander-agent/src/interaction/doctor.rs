//! Read-only, content-safe Runtime environment inspection for an Agent.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorAttention {
    Healthy,
    Active,
    Waiting,
    Recovering,
    NeedsReview,
    ManualActionRequired,
}

/// Bounded operational facts an Agent may use to replan its own workflow.
/// It contains no mutation authority, prompt content, or hidden reasoning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub attention: DoctorAttention,
    pub active_agents: u64,
    pub waiting_agents: u64,
    pub manual_agents: u64,
    pub ready_tasks: u64,
    pub running_tasks: u64,
    pub blocked_tasks: u64,
    pub review_tasks: u64,
    pub remaining_token_budget: u64,
    pub integrating_workspaces: u64,
    pub conflicted_workspaces: u64,
    pub manual_workspaces: u64,
    pub interrupted_models: u64,
    pub interrupted_perceptions: u64,
    pub completed_perceptions: u64,
    pub total_perceptions: u64,
    pub interrupted_tools: u64,
    pub operator_recoveries: u64,
    pub open_arbitrations: u64,
}

#[async_trait]
pub trait DoctorGate: Send + Sync {
    async fn inspect(&self) -> Result<DoctorReport, String>;
}
