//! Deterministic encrypted artifacts for same-Agent auxiliary model calls.

use async_trait::async_trait;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CognitionArtifactKind {
    SourceMedia,
    SourcePrompt,
    ProviderReceipt,
    NormalizedOutput,
}

impl CognitionArtifactKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceMedia => "source_media",
            Self::SourcePrompt => "source_prompt",
            Self::ProviderReceipt => "provider_receipt",
            Self::NormalizedOutput => "normalized_output",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CognitionArtifactRecord {
    pub locator: String,
    pub media_type: String,
    pub payload: Vec<u8>,
    pub digest: String,
}

/// Turn-bound encrypted authority shared by perception and text cognition.
/// Exact `(invocation, kind)` writes are idempotent; changed content conflicts.
#[async_trait]
pub trait CognitionArtifactStore: Send + Sync {
    async fn persist_exact(
        &self,
        invocation_id: &str,
        kind: CognitionArtifactKind,
        media_type: &str,
        payload: Vec<u8>,
    ) -> Result<CognitionArtifactRecord, CognitionArtifactError>;

    async fn load_exact(
        &self,
        invocation_id: &str,
        kind: CognitionArtifactKind,
    ) -> Result<Option<CognitionArtifactRecord>, CognitionArtifactError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CognitionArtifactError {
    #[error("cognition artifact content conflicts with its durable identity")]
    Conflict,
    #[error("cognition artifact storage is unavailable")]
    Unavailable,
}
