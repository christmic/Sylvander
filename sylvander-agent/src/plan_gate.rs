//! Typed pause point for a model-proposed implementation plan.

use async_trait::async_trait;
use sylvander_protocol::PlanDecision;

#[async_trait]
pub trait PlanGate: Send + Sync {
    async fn review(&self, plan_id: &str, steps: Vec<String>) -> PlanDecision;
}
