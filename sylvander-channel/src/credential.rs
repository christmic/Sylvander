//! Transport-neutral, renewable credential lease contract.
//!
//! Channel crates depend on this module rather than on Runtime configuration
//! or secret-provider implementations. Multi-value protocol credentials are
//! returned as one atomic bundle with a shared generation and expiry.

use std::collections::BTreeMap;

use async_trait::async_trait;

const MAX_LEASE_SECONDS: i64 = 300;
const MAX_SLOT_NAME_BYTES: usize = 128;
const MAX_SECRET_BYTES: usize = 64 * 1024;

/// Immutable request for one channel instance's atomic credential bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialLeaseRequest {
    pub instance_id: String,
    pub slots: Vec<String>,
}

impl CredentialLeaseRequest {
    pub fn new(
        instance_id: impl Into<String>,
        slots: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, CredentialLeaseError> {
        let instance_id = instance_id.into();
        validate_name(&instance_id)?;
        let mut slots = slots.into_iter().map(Into::into).collect::<Vec<_>>();
        if slots.is_empty() {
            return Err(CredentialLeaseError::InvalidRequest);
        }
        for slot in &slots {
            validate_name(slot)?;
        }
        slots.sort();
        if slots.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(CredentialLeaseError::InvalidRequest);
        }
        Ok(Self { instance_id, slots })
    }
}

/// One atomic set of short-lived channel credentials.
///
/// Secret allocations clear their bytes on drop. Formatting exposes only
/// non-sensitive lease metadata and the number of slots.
pub struct CredentialLeaseBundle {
    credential_generation: u64,
    lease_generation: u64,
    issued_at_unix_secs: i64,
    expires_at_unix_secs: i64,
    values: BTreeMap<String, ZeroingCredential>,
}

impl CredentialLeaseBundle {
    pub fn new(
        credential_generation: u64,
        lease_generation: u64,
        issued_at_unix_secs: i64,
        expires_at_unix_secs: i64,
        values: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<Self, CredentialLeaseError> {
        if credential_generation == 0
            || lease_generation == 0
            || issued_at_unix_secs < 0
            || expires_at_unix_secs <= issued_at_unix_secs
            || expires_at_unix_secs.saturating_sub(issued_at_unix_secs) > MAX_LEASE_SECONDS
        {
            return Err(CredentialLeaseError::InvalidLease);
        }
        let mut normalized = BTreeMap::new();
        for (slot, value) in values {
            validate_name(&slot)?;
            if normalized
                .insert(slot, ZeroingCredential::new(value)?)
                .is_some()
            {
                return Err(CredentialLeaseError::InvalidLease);
            }
        }
        if normalized.is_empty() {
            return Err(CredentialLeaseError::InvalidLease);
        }
        Ok(Self {
            credential_generation,
            lease_generation,
            issued_at_unix_secs,
            expires_at_unix_secs,
            values: normalized,
        })
    }

    #[must_use]
    pub const fn credential_generation(&self) -> u64 {
        self.credential_generation
    }

    #[must_use]
    pub const fn lease_generation(&self) -> u64 {
        self.lease_generation
    }

    #[must_use]
    pub const fn issued_at_unix_secs(&self) -> i64 {
        self.issued_at_unix_secs
    }

    #[must_use]
    pub const fn expires_at_unix_secs(&self) -> i64 {
        self.expires_at_unix_secs
    }

    /// Access one slot using the process wall clock.
    pub fn secret(&self, slot: &str) -> Result<&str, CredentialLeaseError> {
        self.secret_at(slot, unix_timestamp())
    }

    /// Access one slot at an injected timestamp for deterministic lifecycle
    /// tests. Expiry is checked before slot lookup to avoid stale fallback.
    pub fn secret_at(&self, slot: &str, now_unix_secs: i64) -> Result<&str, CredentialLeaseError> {
        if now_unix_secs >= self.expires_at_unix_secs {
            return Err(CredentialLeaseError::Expired);
        }
        self.values
            .get(slot)
            .ok_or(CredentialLeaseError::MissingSlot)?
            .as_str()
    }

    #[must_use]
    pub fn contains_exact_slots(&self, slots: &[String]) -> bool {
        self.values
            .keys()
            .map(String::as_str)
            .eq(slots.iter().map(String::as_str))
    }
}

impl std::fmt::Debug for CredentialLeaseBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialLeaseBundle")
            .field("credential_generation", &self.credential_generation)
            .field("lease_generation", &self.lease_generation)
            .field("issued_at_unix_secs", &self.issued_at_unix_secs)
            .field("expires_at_unix_secs", &self.expires_at_unix_secs)
            .field("slot_count", &self.values.len())
            .field("values", &"[REDACTED]")
            .finish()
    }
}

/// Runtime-owned source used at each channel operation boundary.
#[async_trait]
pub trait CredentialLeaseSource: Send + Sync {
    async fn lease(
        &self,
        request: &CredentialLeaseRequest,
    ) -> Result<CredentialLeaseBundle, CredentialLeaseError>;
}

/// Content-free error classes safe for transport logs and protocol mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CredentialLeaseError {
    #[error("credential lease request is invalid")]
    InvalidRequest,
    #[error("credential lease is invalid")]
    InvalidLease,
    #[error("credential lease is unavailable")]
    Unavailable,
    #[error("credential lease expired")]
    Expired,
    #[error("credential lease is missing a required slot")]
    MissingSlot,
    #[error("credential lease value is not UTF-8")]
    InvalidEncoding,
}

struct ZeroingCredential(Vec<u8>);

impl ZeroingCredential {
    fn new(bytes: Vec<u8>) -> Result<Self, CredentialLeaseError> {
        if bytes.is_empty() || bytes.len() > MAX_SECRET_BYTES {
            return Err(CredentialLeaseError::InvalidLease);
        }
        std::str::from_utf8(&bytes).map_err(|_| CredentialLeaseError::InvalidEncoding)?;
        Ok(Self(bytes))
    }

    fn as_str(&self) -> Result<&str, CredentialLeaseError> {
        std::str::from_utf8(&self.0).map_err(|_| CredentialLeaseError::InvalidEncoding)
    }
}

impl Drop for ZeroingCredential {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl std::fmt::Debug for ZeroingCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ZeroingCredential([REDACTED])")
    }
}

fn validate_name(value: &str) -> Result<(), CredentialLeaseError> {
    if value.is_empty()
        || value.len() > MAX_SLOT_NAME_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(CredentialLeaseError::InvalidRequest);
    }
    Ok(())
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
