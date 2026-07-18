//! Runtime adapter from configured secret references to channel leases.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sylvander_channel::credential::{
    CredentialLeaseBundle, CredentialLeaseError, CredentialLeaseRequest, CredentialLeaseSource,
};
use sylvander_runtime::config::{SecretRef, SecretResolver};
use sylvander_runtime::credential_audit::{
    CredentialAuditOperation, CredentialAuditResult, CredentialAuditSubject,
    CredentialOperationAuditLedger,
};
use tokio::sync::Mutex;

const LEASE_SECONDS: i64 = 30;

/// One channel-instance-scoped source. Values are resolved together for every
/// operation, so a partial resolution failure cannot publish a mixed bundle.
pub(crate) struct SystemChannelCredentialSource {
    instance_id: String,
    references: BTreeMap<String, SecretRef>,
    resolver: Arc<dyn SecretResolver>,
    audit: Arc<CredentialOperationAuditLedger>,
    audit_subject: CredentialAuditSubject,
    state: Mutex<CredentialState>,
    next_lease_generation: AtomicU64,
}

#[derive(Default)]
struct CredentialState {
    generation: u64,
    fingerprints: HashMap<String, [u8; 32]>,
}

impl SystemChannelCredentialSource {
    pub(crate) fn new(
        instance_id: impl Into<String>,
        references: impl IntoIterator<Item = (String, SecretRef)>,
        resolver: Arc<dyn SecretResolver>,
        audit: Arc<CredentialOperationAuditLedger>,
    ) -> Result<Self, CredentialLeaseError> {
        let pairs = references.into_iter().collect::<Vec<_>>();
        let pair_count = pairs.len();
        let references = pairs.into_iter().collect::<BTreeMap<_, _>>();
        if references.len() != pair_count {
            return Err(CredentialLeaseError::InvalidRequest);
        }
        Self::from_map(instance_id, references, resolver, audit)
    }

    pub(crate) fn from_map(
        instance_id: impl Into<String>,
        references: BTreeMap<String, SecretRef>,
        resolver: Arc<dyn SecretResolver>,
        audit: Arc<CredentialOperationAuditLedger>,
    ) -> Result<Self, CredentialLeaseError> {
        let instance_id = instance_id.into();
        let request = CredentialLeaseRequest::new(instance_id.clone(), references.keys().cloned())?;
        if !request
            .slots
            .iter()
            .all(|slot| references.contains_key(slot))
        {
            return Err(CredentialLeaseError::InvalidRequest);
        }
        Ok(Self {
            audit_subject: CredentialAuditSubject::channel_instance(instance_id.clone())
                .map_err(|_| CredentialLeaseError::InvalidRequest)?,
            instance_id,
            references,
            resolver,
            audit,
            state: Mutex::new(CredentialState::default()),
            next_lease_generation: AtomicU64::new(1),
        })
    }
}

impl std::fmt::Debug for SystemChannelCredentialSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SystemChannelCredentialSource")
            .field("instance_id", &self.instance_id)
            .field("slot_count", &self.references.len())
            .field("references", &"[REDACTED]")
            // Resolver and mutable lease state intentionally remain opaque.
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl CredentialLeaseSource for SystemChannelCredentialSource {
    async fn lease(
        &self,
        request: &CredentialLeaseRequest,
    ) -> Result<CredentialLeaseBundle, CredentialLeaseError> {
        if request.instance_id != self.instance_id
            || request
                .slots
                .iter()
                .any(|slot| !self.references.contains_key(slot))
        {
            self.record_failure(None, CredentialAuditResult::InvalidRequest)
                .await;
            return Err(CredentialLeaseError::Unavailable);
        }

        // Serialize resolve+publish for this instance. A slower operation can
        // therefore never overwrite a newer observed generation.
        let mut state = self.state.lock().await;
        let mut values = BTreeMap::new();
        let mut fingerprints = HashMap::new();
        for slot in &request.slots {
            let Some(reference) = self.references.get(slot) else {
                self.record_failure(
                    nonzero(state.generation),
                    CredentialAuditResult::MissingSlot,
                )
                .await;
                return Err(CredentialLeaseError::Unavailable);
            };
            let Ok(secret) = self.resolver.resolve(reference) else {
                self.record_failure(
                    nonzero(state.generation),
                    CredentialAuditResult::Unavailable,
                )
                .await;
                return Err(CredentialLeaseError::Unavailable);
            };
            let bytes = secret.as_bytes().to_vec();
            fingerprints.insert(slot.clone(), Sha256::digest(&bytes).into());
            values.insert(slot.clone(), bytes);
            drop(secret);
        }

        let changed = request
            .slots
            .iter()
            .any(|slot| state.fingerprints.get(slot) != fingerprints.get(slot));
        let operation = if state.generation == 0 {
            CredentialAuditOperation::Create
        } else if changed {
            CredentialAuditOperation::Rotate
        } else {
            CredentialAuditOperation::Renew
        };
        let credential_generation = if state.generation == 0 || changed {
            let Some(generation) = state.generation.checked_add(1) else {
                self.record_failure(
                    nonzero(state.generation),
                    CredentialAuditResult::Unavailable,
                )
                .await;
                return Err(CredentialLeaseError::Unavailable);
            };
            generation
        } else {
            state.generation
        };

        let Ok(lease_generation) = self.next_lease_generation.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |current| current.checked_add(1),
        ) else {
            self.record_failure(
                Some(credential_generation),
                CredentialAuditResult::Unavailable,
            )
            .await;
            return Err(CredentialLeaseError::Unavailable);
        };
        let now = unix_timestamp();
        let bundle = match CredentialLeaseBundle::new(
            credential_generation,
            lease_generation,
            now,
            now.saturating_add(LEASE_SECONDS),
            values,
        ) {
            Ok(bundle) => bundle,
            Err(error) => {
                self.record_failure(Some(credential_generation), audit_result(error))
                    .await;
                return Err(error);
            }
        };
        if !bundle.contains_exact_slots(&request.slots) {
            self.record_failure(
                Some(credential_generation),
                CredentialAuditResult::InvalidLease,
            )
            .await;
            return Err(CredentialLeaseError::InvalidLease);
        }
        self.audit
            .record(
                &self.audit_subject,
                operation,
                Some(credential_generation),
                CredentialAuditResult::Succeeded,
            )
            .await
            .map_err(|_| CredentialLeaseError::Unavailable)?;
        state.generation = credential_generation;
        for (slot, fingerprint) in fingerprints {
            state.fingerprints.insert(slot, fingerprint);
        }
        drop(state);
        tracing::debug!(
            instance = %self.instance_id,
            slot_count = request.slots.len(),
            credential_generation,
            lease_generation,
            expires_at = bundle.expires_at_unix_secs(),
            "channel credential lease opened"
        );
        Ok(bundle)
    }
}

impl SystemChannelCredentialSource {
    async fn record_failure(
        &self,
        credential_revision: Option<u64>,
        result: CredentialAuditResult,
    ) {
        let _ = self
            .audit
            .record(
                &self.audit_subject,
                CredentialAuditOperation::Failure,
                credential_revision,
                result,
            )
            .await;
    }
}

const fn nonzero(value: u64) -> Option<u64> {
    if value == 0 { None } else { Some(value) }
}

const fn audit_result(error: CredentialLeaseError) -> CredentialAuditResult {
    match error {
        CredentialLeaseError::InvalidRequest => CredentialAuditResult::InvalidRequest,
        CredentialLeaseError::InvalidLease => CredentialAuditResult::InvalidLease,
        CredentialLeaseError::Unavailable => CredentialAuditResult::Unavailable,
        CredentialLeaseError::Expired => CredentialAuditResult::Expired,
        CredentialLeaseError::MissingSlot => CredentialAuditResult::MissingSlot,
        CredentialLeaseError::InvalidEncoding => CredentialAuditResult::InvalidEncoding,
    }
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
#[path = "../tests/unit/credential.rs"]
mod tests;
