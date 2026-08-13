//! Location-neutral retention port for artifacts produced during one turn.
//!
//! Runtime binds an implementation to the authenticated user, Agent, Session,
//! and turn before constructing [`AgentExecutionPorts`](crate::execution::ports::AgentExecutionPorts).
//! Agent can submit content and correlation metadata, but it cannot select a
//! backend, path, tenant, retention rule, or encryption key.

use async_trait::async_trait;

/// Content submitted for immutable retention outside model context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactWrite {
    /// Tool-call correlation. Runtime must not interpret this as a path.
    pub call_id: String,
    /// Explicit Internet media type, initially `text/plain; charset=utf-8`.
    pub media_type: String,
    /// Exact content retained before the model-visible value is shortened.
    pub payload: Vec<u8>,
}

/// Location-neutral reference returned after durable persistence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactReference {
    /// Opaque Runtime locator. It is a reference, not filesystem authority.
    pub locator: String,
    /// Number of source bytes accepted by the backend.
    pub original_bytes: usize,
}

/// Bounded failure classes safe for Agent policy and events.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ArtifactStoreError {
    /// The submitted metadata or payload violates the backend contract.
    #[error("artifact request was rejected")]
    InvalidRequest,
    /// The configured backend could not durably retain the content.
    #[error("artifact storage is unavailable")]
    Unavailable,
}

/// Runtime-selected artifact authority bound to exactly one Agent turn.
#[async_trait]
pub trait TurnArtifactStore: Send + Sync {
    /// Persist one immutable artifact and return an opaque reference.
    async fn persist(
        &self,
        artifact: ArtifactWrite,
    ) -> Result<ArtifactReference, ArtifactStoreError>;
}

#[cfg(test)]
#[path = "../../tests/unit/artifact.rs"]
mod tests;
