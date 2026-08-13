//! Bounded same-Agent consultation selected by the primary model.
//!
//! A cognition role has no Agent identity, tools, mailbox, memory, workspace,
//! or authority. Runtime decides whether the requested role is approved and
//! owns execution, persistence, recovery, cost, and observation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitionRole {
    FastDraft,
    Deliberation,
    Critic,
}

/// Model-authored advisory intent. Runtime adds the stable invocation identity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CognitionIntent {
    pub role: CognitionRole,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CognitionRequest {
    pub invocation_id: String,
    pub role: CognitionRole,
    pub prompt: String,
}

/// Bounded advisory text. The primary model remains the final answer authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CognitionObservation {
    pub role: CognitionRole,
    pub text: String,
}

#[async_trait]
pub trait CognitionGate: Send + Sync {
    async fn consult(&self, request: CognitionRequest) -> Result<CognitionObservation, String>;
}
