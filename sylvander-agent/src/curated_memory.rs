//! Runtime-owned bridge between Worker memory proposals and curated context.
//!
//! Worker tools can propose typed candidates, but they cannot choose owners,
//! issue Guardian authority, or write canonical stores. The Runtime implements
//! these traits and derives all ownership from
//! [`ToolContext`](crate::tool_context::ToolContext).

use async_trait::async_trait;
use sylvander_protocol::{AgentId, SessionId, UserId};

use crate::tool_context::ToolContext;

/// Governed destination proposed by a Worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CuratedMemoryScope {
    Relationship,
    UserProfile,
    AgentCanonical,
    WorkspaceKnowledge,
}

/// Bounded semantic payload proposed by the Worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCandidateSubmission {
    pub scope: CuratedMemoryScope,
    pub content: String,
    pub tags: Vec<String>,
}

/// Durable acknowledgement returned after the Runtime has staged the payload
/// and enqueued its `MemoryCandidateCreated` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCandidateReceipt {
    pub event_id: String,
}

/// Fail-closed candidate submission error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MemoryCandidateError {
    #[error("memory candidate is invalid")]
    Invalid,
    #[error("memory candidate access denied")]
    AccessDenied,
    #[error("memory candidate service unavailable")]
    Unavailable,
}

/// Runtime-owned candidate ingress. Implementations must derive owner,
/// workspace, evidence, and retention from trusted Runtime state.
#[async_trait]
pub trait MemoryCandidateSink: Send + Sync {
    async fn submit(
        &self,
        context: &ToolContext,
        candidate: MemoryCandidateSubmission,
    ) -> Result<MemoryCandidateReceipt, MemoryCandidateError>;
}

/// Runtime-derived subject for curated retrieval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratedContextSubject {
    pub user_id: UserId,
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub workspace_ids: Vec<String>,
}

/// One governed value returned to the typed turn-context composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratedContextEntry {
    pub scope: CuratedMemoryScope,
    pub content: String,
    pub reference: String,
    pub revision: u64,
    pub expires_at_unix_secs: Option<i64>,
    pub relevance: u16,
}

/// Runtime-owned retrieval boundary for committed Guardian output.
#[async_trait]
pub trait CuratedContextProvider: Send + Sync {
    async fn retrieve(
        &self,
        subject: &CuratedContextSubject,
        query: &str,
        max_items: usize,
    ) -> Result<Vec<CuratedContextEntry>, MemoryCandidateError>;
}

#[cfg(test)]
#[path = "../tests/unit/curated_memory.rs"]
mod tests;
