//! Typed pause point for a model-proposed implementation plan.

use async_trait::async_trait;

/// Agent-owned result of reviewing a model-proposed plan.
///
/// This is execution-domain state rather than a client DTO. Runtime maps
/// authenticated API decisions into this enum before releasing the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanDecision {
    Approved,
    Revised { steps: Vec<String> },
    Rejected { reason: String },
}

#[async_trait]
pub trait PlanGate: Send + Sync {
    async fn review(&self, plan_id: &str, steps: Vec<String>) -> PlanDecision;
    async fn update(&self, plan_id: &str, steps: Vec<String>, current: usize);
}
