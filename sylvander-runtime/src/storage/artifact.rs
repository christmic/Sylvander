//! Governed Runtime implementation of Agent's turn-bound artifact port.
//!
//! The service binds authenticated product identity before handing authority to
//! Agent. Persist requests contain only content and tool-call correlation; the
//! adapter derives an opaque record identifier and a content-safe source digest.

use std::sync::Arc;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sylvander_agent::artifact::{
    ArtifactReference, ArtifactStoreError, ArtifactWrite, TurnArtifactStore,
};

use crate::evidence::{
    EvidenceClassification, EvidenceError, EvidenceStore, GovernedRecordInput, GovernedRecordKind,
};

/// Identity and time fixed by Runtime for one admitted Agent turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ArtifactTurnBinding {
    pub(crate) user_id: String,
    pub(crate) agent_id: String,
    pub(crate) session_id: String,
    pub(crate) turn_id: String,
    pub(crate) created_at: i64,
}

/// Factory over the encrypted governed store selected during Runtime boot.
#[derive(Clone)]
pub(crate) struct RuntimeArtifactService {
    store: EvidenceStore,
}

impl RuntimeArtifactService {
    /// Accept only an encryption-enabled store; plaintext fallback is forbidden.
    pub(crate) fn new(store: EvidenceStore) -> Result<Self, EvidenceError> {
        if !store.governance_enabled() {
            return Err(EvidenceError::EncryptionRequired);
        }
        Ok(Self { store })
    }

    /// Bind one unforgeable Agent port to the admitted turn identity.
    pub(crate) fn bind(
        &self,
        binding: ArtifactTurnBinding,
    ) -> Result<Arc<dyn TurnArtifactStore>, EvidenceError> {
        if binding.created_at < 0
            || binding.agent_id.is_empty()
            || binding.session_id.is_empty()
            || binding.turn_id.is_empty()
        {
            return Err(EvidenceError::InvalidGovernedRecord);
        }
        let source_seed = source_seed(&binding);
        let scope = self.store.governed_scope(binding.user_id)?;
        Ok(Arc::new(BoundArtifactStore {
            store: self.store.clone(),
            scope,
            source_seed,
            created_at: binding.created_at,
        }))
    }
}

struct BoundArtifactStore {
    store: EvidenceStore,
    scope: crate::evidence::EvidenceScope,
    source_seed: String,
    created_at: i64,
}

#[async_trait]
impl TurnArtifactStore for BoundArtifactStore {
    async fn persist(
        &self,
        artifact: ArtifactWrite,
    ) -> Result<ArtifactReference, ArtifactStoreError> {
        let original_bytes = artifact.payload.len();
        let id = uuid::Uuid::new_v4().to_string();
        let source_ref = artifact_source(&self.source_seed, &artifact.call_id);
        self.store
            .put_governed_record(GovernedRecordInput {
                id: id.clone(),
                scope: self.scope.clone(),
                kind: GovernedRecordKind::Artifact,
                classification: EvidenceClassification::Restricted,
                source_ref,
                media_type: artifact.media_type,
                payload: artifact.payload,
                created_at: self.created_at,
            })
            .await
            .map_err(map_store_error)?;
        Ok(ArtifactReference {
            locator: format!("artifact:{id}"),
            original_bytes,
        })
    }
}

fn source_seed(binding: &ArtifactTurnBinding) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sylvander-agent-artifact-turn-v1\0");
    for value in [&binding.agent_id, &binding.session_id, &binding.turn_id] {
        update_length_prefixed(&mut digest, value);
    }
    format!("agent-turn:sha256:{:x}", digest.finalize())
}

fn artifact_source(source_seed: &str, call_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sylvander-agent-artifact-call-v1\0");
    update_length_prefixed(&mut digest, call_id);
    format!("{source_seed}:call-sha256:{:x}", digest.finalize())
}

fn update_length_prefixed(digest: &mut Sha256, value: &str) {
    digest.update(value.len().to_string().as_bytes());
    digest.update(b":");
    digest.update(value.as_bytes());
}

fn map_store_error(error: EvidenceError) -> ArtifactStoreError {
    match error {
        EvidenceError::InvalidGovernedRecord | EvidenceError::EvidenceScopeMismatch => {
            ArtifactStoreError::InvalidRequest
        }
        _ => ArtifactStoreError::Unavailable,
    }
}

#[cfg(test)]
#[path = "../../tests/unit/runtime_artifact.rs"]
mod tests;
