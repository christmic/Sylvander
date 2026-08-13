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

use crate::agent::cognition_artifact::{
    CognitionArtifactError, CognitionArtifactKind, CognitionArtifactRecord, CognitionArtifactStore,
};
use crate::evidence::{
    EvidenceClassification, EvidenceError, EvidenceStore, GovernedArtifactRange,
    GovernedRecordInput, GovernedRecordKind, artifact_session_source_prefix,
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
        let source_seed = source_seed(&binding)?;
        let scope = self.store.governed_scope(binding.user_id)?;
        Ok(Arc::new(BoundArtifactStore {
            store: self.store.clone(),
            scope,
            source_seed,
            created_at: binding.created_at,
        }))
    }

    /// Bind deterministic encrypted storage for one turn's perception calls.
    pub(crate) fn bind_cognition(
        &self,
        binding: ArtifactTurnBinding,
    ) -> Result<Arc<dyn CognitionArtifactStore>, EvidenceError> {
        if binding.created_at < 0
            || binding.agent_id.is_empty()
            || binding.session_id.is_empty()
            || binding.turn_id.is_empty()
        {
            return Err(EvidenceError::InvalidGovernedRecord);
        }
        let source_seed = source_seed(&binding);
        Ok(Arc::new(BoundArtifactStore {
            scope: self.store.governed_scope(binding.user_id)?,
            source_seed,
            store: self.store.clone(),
            created_at: binding.created_at,
        }))
    }

    /// Resolve one opaque locator inside the authenticated user and Session.
    pub(crate) async fn read_range(
        &self,
        user_id: String,
        session_id: String,
        locator: &str,
        offset: usize,
        max_bytes: usize,
        read_at: i64,
    ) -> Result<GovernedArtifactRange, EvidenceError> {
        let record_id = parse_locator(locator)?;
        self.store
            .read_governed_artifact_range(
                self.store.governed_scope(user_id)?,
                record_id.to_string(),
                session_id,
                offset,
                max_bytes,
                read_at,
            )
            .await
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

#[async_trait]
impl CognitionArtifactStore for BoundArtifactStore {
    async fn persist_exact(
        &self,
        invocation_id: &str,
        kind: CognitionArtifactKind,
        media_type: &str,
        payload: Vec<u8>,
    ) -> Result<CognitionArtifactRecord, CognitionArtifactError> {
        let id = perception_record_id(invocation_id, kind);
        if let Some(existing) = self.load_perception_record(invocation_id, kind).await? {
            return matching_artifact(existing, media_type, &payload);
        }
        let source_ref = perception_source(&self.source_seed, invocation_id, kind);
        let write = self
            .store
            .put_governed_record(GovernedRecordInput {
                id: id.clone(),
                scope: self.scope.clone(),
                kind: GovernedRecordKind::Artifact,
                classification: EvidenceClassification::Restricted,
                source_ref,
                media_type: media_type.to_owned(),
                payload: payload.clone(),
                created_at: self.created_at,
            })
            .await;
        if write.is_err() {
            if let Some(existing) = self.load_perception_record(invocation_id, kind).await? {
                return matching_artifact(existing, media_type, &payload);
            }
            return Err(CognitionArtifactError::Unavailable);
        }
        self.load_perception_record(invocation_id, kind)
            .await?
            .ok_or(CognitionArtifactError::Unavailable)
    }

    async fn load_exact(
        &self,
        invocation_id: &str,
        kind: CognitionArtifactKind,
    ) -> Result<Option<CognitionArtifactRecord>, CognitionArtifactError> {
        self.load_perception_record(invocation_id, kind).await
    }
}

impl BoundArtifactStore {
    async fn load_perception_record(
        &self,
        invocation_id: &str,
        kind: CognitionArtifactKind,
    ) -> Result<Option<CognitionArtifactRecord>, CognitionArtifactError> {
        let id = perception_record_id(invocation_id, kind);
        let export = self
            .store
            .export_governed_records(self.scope.clone(), vec![id.clone()], self.created_at)
            .await;
        let export = match export {
            Ok(export) => export,
            Err(EvidenceError::GovernedRecordNotFound) => return Ok(None),
            Err(_) => return Err(CognitionArtifactError::Unavailable),
        };
        let record = export
            .records
            .into_iter()
            .next()
            .ok_or(CognitionArtifactError::Unavailable)?;
        if record.id != id
            || record.kind != GovernedRecordKind::Artifact
            || record.source_ref != perception_source(&self.source_seed, invocation_id, kind)
        {
            return Err(CognitionArtifactError::Conflict);
        }
        Ok(Some(CognitionArtifactRecord {
            locator: format!("artifact:{}", record.id),
            media_type: record.media_type,
            payload: record.payload,
            digest: format!("sha256:{}", record.payload_digest_sha256),
        }))
    }
}

fn matching_artifact(
    existing: CognitionArtifactRecord,
    media_type: &str,
    payload: &[u8],
) -> Result<CognitionArtifactRecord, CognitionArtifactError> {
    if existing.media_type == media_type && existing.payload == payload {
        Ok(existing)
    } else {
        Err(CognitionArtifactError::Conflict)
    }
}

fn perception_record_id(invocation_id: &str, kind: CognitionArtifactKind) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!(
            "sylvander-perception-artifact-v1:{}:{}",
            invocation_id,
            kind.as_str()
        )
        .as_bytes(),
    )
    .to_string()
}

fn perception_source(
    source_seed: &str,
    invocation_id: &str,
    kind: CognitionArtifactKind,
) -> String {
    format!(
        "{source_seed}:perception:{}:{}",
        invocation_id,
        kind.as_str()
    )
}

fn source_seed(binding: &ArtifactTurnBinding) -> Result<String, EvidenceError> {
    let mut digest = Sha256::new();
    digest.update(b"sylvander-agent-artifact-turn-v1\0");
    for value in [&binding.agent_id, &binding.session_id, &binding.turn_id] {
        update_length_prefixed(&mut digest, value);
    }
    let session_prefix = artifact_session_source_prefix(&binding.session_id)?;
    Ok(format!(
        "{session_prefix}agent-turn-sha256:{:x}",
        digest.finalize()
    ))
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

fn parse_locator(locator: &str) -> Result<&str, EvidenceError> {
    let id = locator
        .strip_prefix("artifact:")
        .or_else(|| locator.strip_prefix("evidence-artifact:"))
        .ok_or(EvidenceError::GovernedRecordNotFound)?;
    if id.is_empty() || id.contains(':') {
        return Err(EvidenceError::GovernedRecordNotFound);
    }
    Ok(id)
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
