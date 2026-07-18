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
use tokio::sync::Mutex;

const LEASE_SECONDS: i64 = 30;

/// One channel-instance-scoped source. Values are resolved together for every
/// operation, so a partial resolution failure cannot publish a mixed bundle.
pub(crate) struct SystemChannelCredentialSource {
    instance_id: String,
    references: BTreeMap<String, SecretRef>,
    resolver: Arc<dyn SecretResolver>,
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
    ) -> Result<Self, CredentialLeaseError> {
        let pairs = references.into_iter().collect::<Vec<_>>();
        let pair_count = pairs.len();
        let references = pairs.into_iter().collect::<BTreeMap<_, _>>();
        if references.len() != pair_count {
            return Err(CredentialLeaseError::InvalidRequest);
        }
        Self::from_map(instance_id, references, resolver)
    }

    pub(crate) fn from_map(
        instance_id: impl Into<String>,
        references: BTreeMap<String, SecretRef>,
        resolver: Arc<dyn SecretResolver>,
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
            instance_id,
            references,
            resolver,
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
            return Err(CredentialLeaseError::Unavailable);
        }

        // Serialize resolve+publish for this instance. A slower operation can
        // therefore never overwrite a newer observed generation.
        let mut state = self.state.lock().await;
        let mut values = BTreeMap::new();
        let mut fingerprints = HashMap::new();
        for slot in &request.slots {
            let secret = self
                .resolver
                .resolve(
                    self.references
                        .get(slot)
                        .ok_or(CredentialLeaseError::Unavailable)?,
                )
                .map_err(|_| CredentialLeaseError::Unavailable)?;
            let bytes = secret.as_bytes().to_vec();
            fingerprints.insert(slot.clone(), Sha256::digest(&bytes).into());
            values.insert(slot.clone(), bytes);
            drop(secret);
        }

        let changed = request
            .slots
            .iter()
            .any(|slot| state.fingerprints.get(slot) != fingerprints.get(slot));
        if state.generation == 0 || changed {
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or(CredentialLeaseError::Unavailable)?;
        }
        for (slot, fingerprint) in fingerprints {
            state.fingerprints.insert(slot, fingerprint);
        }
        let credential_generation = state.generation;
        drop(state);

        let lease_generation = self
            .next_lease_generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| CredentialLeaseError::Unavailable)?;
        let now = unix_timestamp();
        let bundle = CredentialLeaseBundle::new(
            credential_generation,
            lease_generation,
            now,
            now.saturating_add(LEASE_SECONDS),
            values,
        )?;
        if !bundle.contains_exact_slots(&request.slots) {
            return Err(CredentialLeaseError::InvalidLease);
        }
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
