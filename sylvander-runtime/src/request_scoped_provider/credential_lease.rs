//! Renewable, short-lived credential leases used by request-scoped providers.
//!
//! Mutable secret-provider state is deliberately kept outside immutable
//! Provider definitions. Every request first reads the active registry
//! generation, then acquires or renews a bounded lease. Expired or failed
//! renewals never fall back to previously cached bytes.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;

use super::{
    ActiveCredentialLease, ActiveCredentialSource, CredentialAccessError, CredentialLeaseFuture,
};
use crate::agent_registry::AgentRegistry;
use crate::config::SecretRef;
use crate::credential_audit::{
    CredentialAuditOperation, CredentialAuditResult, CredentialAuditSubject,
    CredentialOperationAuditLedger,
};
use crate::credential_registry::CredentialSecretResolver;

/// Maximum lease lifetime accepted from an external provider.
pub const MAX_EXTERNAL_SECRET_LEASE_SECONDS: i64 = 300;
const DEFAULT_LEASE_SECONDS: i64 = 30;
const DEFAULT_RENEW_BEFORE_SECONDS: i64 = 30;

/// Boxed acquire/renew result returned by an external secret provider.
pub type ExternalSecretLeaseFuture<'a> = Pin<
    Box<dyn Future<Output = Result<ExternalSecretLease, ExternalSecretLeaseError>> + Send + 'a>,
>;

/// Metadata passed to an external provider when renewing a live lease.
///
/// It contains no secret bytes or secret-provider locator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretLeaseMetadata {
    /// Active immutable credential-registry generation.
    pub credential_generation: u64,
    /// Monotonic generation assigned by the external lease provider.
    pub lease_generation: u64,
    /// Lease issuance time as Unix seconds.
    pub issued_at_unix_secs: i64,
    /// Exclusive lease expiry time as Unix seconds.
    pub expires_at_unix_secs: i64,
}

/// One bounded lease returned by an external secret provider.
///
/// Formatting is intentionally content-free. Secret bytes are owned by a
/// zeroing allocation and copied only into equally zeroing request leases.
pub struct ExternalSecretLease {
    metadata: SecretLeaseMetadata,
    secret: ZeroingSecret,
}

impl ExternalSecretLease {
    /// Construct a bounded external lease. Secret bytes must be non-empty
    /// UTF-8 and are cleared when the lease is dropped.
    pub fn new(
        credential_generation: u64,
        lease_generation: u64,
        issued_at_unix_secs: i64,
        expires_at_unix_secs: i64,
        secret: impl Into<Vec<u8>>,
    ) -> Result<Self, ExternalSecretLeaseError> {
        if credential_generation == 0
            || lease_generation == 0
            || issued_at_unix_secs < 0
            || expires_at_unix_secs <= issued_at_unix_secs
            || expires_at_unix_secs.saturating_sub(issued_at_unix_secs)
                > MAX_EXTERNAL_SECRET_LEASE_SECONDS
        {
            return Err(ExternalSecretLeaseError::InvalidLease);
        }
        let secret = ZeroingSecret::new(secret.into())?;
        secret.as_str()?;
        Ok(Self {
            metadata: SecretLeaseMetadata {
                credential_generation,
                lease_generation,
                issued_at_unix_secs,
                expires_at_unix_secs,
            },
            secret,
        })
    }

    /// Return content-free generation and lifetime metadata.
    pub const fn metadata(&self) -> SecretLeaseMetadata {
        self.metadata
    }

    fn request_lease(
        &self,
        clock: Arc<dyn LeaseClock>,
    ) -> Result<RequestCredentialLease, CredentialAccessError> {
        if clock.now_unix_secs() >= self.metadata.expires_at_unix_secs {
            return Err(CredentialAccessError::Expired);
        }
        Ok(RequestCredentialLease {
            metadata: self.metadata,
            secret: self.secret.duplicate(),
            clock,
        })
    }
}

impl std::fmt::Debug for ExternalSecretLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ExternalSecretLease([REDACTED])")
    }
}

/// Provider boundary for Vault-like acquire and renew operations.
///
/// Implementations must return a fresh lease generation on renewal. Runtime
/// validates generation, expiry, and maximum TTL independently.
pub trait RenewableExternalSecretProvider: Send + Sync {
    /// Acquire the current external lease for one active registry generation.
    fn acquire<'a>(
        &'a self,
        reference: &'a SecretRef,
        credential_generation: u64,
        now_unix_secs: i64,
    ) -> ExternalSecretLeaseFuture<'a>;

    /// Renew a live lease. Implementations must return a strictly newer
    /// external lease generation while preserving the credential generation.
    fn renew<'a>(
        &'a self,
        reference: &'a SecretRef,
        current: SecretLeaseMetadata,
        now_unix_secs: i64,
    ) -> ExternalSecretLeaseFuture<'a>;
}

/// Stable, redacted failure classes safe to map into provider errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ExternalSecretLeaseError {
    #[error("external secret lease is unavailable")]
    Unavailable,
    #[error("external secret lease is invalid")]
    InvalidLease,
}

/// Adapter for current environment/file references.
///
/// Each renew re-reads the reference, so file-backed rotations become visible
/// without rebuilding an Agent or Provider adapter.
struct SystemExternalSecretProvider {
    resolver: Arc<dyn CredentialSecretResolver>,
    next_lease_generation: AtomicU64,
    lease_seconds: i64,
}

impl SystemExternalSecretProvider {
    fn new(resolver: Arc<dyn CredentialSecretResolver>) -> Self {
        Self {
            resolver,
            next_lease_generation: AtomicU64::new(1),
            lease_seconds: DEFAULT_LEASE_SECONDS,
        }
    }

    fn issue(
        &self,
        reference: &SecretRef,
        credential_generation: u64,
        now_unix_secs: i64,
    ) -> Result<ExternalSecretLease, ExternalSecretLeaseError> {
        let secret = self
            .resolver
            .resolve_credential(reference)
            .map_err(|()| ExternalSecretLeaseError::Unavailable)?;
        let lease_generation = self
            .next_lease_generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| ExternalSecretLeaseError::Unavailable)?;
        let result = ExternalSecretLease::new(
            credential_generation,
            lease_generation,
            now_unix_secs,
            now_unix_secs.saturating_add(self.lease_seconds),
            secret.as_bytes().to_vec(),
        );
        drop(secret);
        result
    }
}

impl std::fmt::Debug for SystemExternalSecretProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SystemExternalSecretProvider")
            .finish_non_exhaustive()
    }
}

impl RenewableExternalSecretProvider for SystemExternalSecretProvider {
    fn acquire<'a>(
        &'a self,
        reference: &'a SecretRef,
        credential_generation: u64,
        now_unix_secs: i64,
    ) -> ExternalSecretLeaseFuture<'a> {
        Box::pin(async move { self.issue(reference, credential_generation, now_unix_secs) })
    }

    fn renew<'a>(
        &'a self,
        reference: &'a SecretRef,
        current: SecretLeaseMetadata,
        now_unix_secs: i64,
    ) -> ExternalSecretLeaseFuture<'a> {
        Box::pin(async move { self.issue(reference, current.credential_generation, now_unix_secs) })
    }
}

pub(crate) trait LeaseClock: Send + Sync {
    fn now_unix_secs(&self) -> i64;
}

#[derive(Debug, Default)]
struct SystemLeaseClock;

impl LeaseClock for SystemLeaseClock {
    fn now_unix_secs(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .try_into()
            .unwrap_or(i64::MAX)
    }
}

/// Registry-backed source used by the real Provider request path.
#[derive(Clone)]
pub(crate) struct RegistryCredentialSource {
    registry: AgentRegistry,
    provider: Arc<dyn RenewableExternalSecretProvider>,
    clock: Arc<dyn LeaseClock>,
    renew_before_seconds: i64,
    leases: Arc<Mutex<HashMap<String, ExternalSecretLease>>>,
    audit: Arc<CredentialOperationAuditLedger>,
}

impl RegistryCredentialSource {
    pub(crate) fn new(
        registry: AgentRegistry,
        resolver: Arc<dyn CredentialSecretResolver>,
        audit: Arc<CredentialOperationAuditLedger>,
    ) -> Self {
        Self::with_provider(
            registry,
            Arc::new(SystemExternalSecretProvider::new(resolver)),
            Arc::new(SystemLeaseClock),
            DEFAULT_RENEW_BEFORE_SECONDS,
            audit,
        )
        .expect("built-in credential lease policy is valid")
    }

    pub(crate) fn with_provider(
        registry: AgentRegistry,
        provider: Arc<dyn RenewableExternalSecretProvider>,
        clock: Arc<dyn LeaseClock>,
        renew_before_seconds: i64,
        audit: Arc<CredentialOperationAuditLedger>,
    ) -> Result<Self, CredentialAccessError> {
        if !(0..=MAX_EXTERNAL_SECRET_LEASE_SECONDS).contains(&renew_before_seconds) {
            return Err(CredentialAccessError::Unavailable);
        }
        Ok(Self {
            registry,
            provider,
            clock,
            renew_before_seconds,
            leases: Arc::new(Mutex::new(HashMap::new())),
            audit,
        })
    }

    pub(crate) fn with_external_provider(
        registry: AgentRegistry,
        provider: Arc<dyn RenewableExternalSecretProvider>,
        audit: Arc<CredentialOperationAuditLedger>,
    ) -> Self {
        Self::with_provider(
            registry,
            provider,
            Arc::new(SystemLeaseClock),
            DEFAULT_RENEW_BEFORE_SECONDS,
            audit,
        )
        .expect("built-in external credential lease policy is valid")
    }

    async fn resolve(
        &self,
        provider_id: &str,
        binding_id: &str,
    ) -> Result<RequestCredentialLease, CredentialAccessError> {
        let subject = CredentialAuditSubject::provider(provider_id, binding_id)
            .map_err(|_| CredentialAccessError::Unavailable)?;
        let stored = match self.registry.load_active_credential(binding_id).await {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                self.record_failure(&subject, None, CredentialAuditResult::Unavailable)
                    .await;
                return Err(CredentialAccessError::Unavailable);
            }
            Err(error) => {
                let error = CredentialAccessError::from_registry(error);
                self.record_failure(&subject, None, audit_result(error))
                    .await;
                return Err(error);
            }
        };
        let reference = &stored.definition.reference;
        let credential_generation = stored.definition.generation;
        let now = self.clock.now_unix_secs();
        let mut leases = self.leases.lock().await;

        let previous = leases.get(binding_id);
        let (replacement, operation) = match previous {
            Some(current)
                if current.metadata.credential_generation == credential_generation
                    && now < current.metadata.expires_at_unix_secs
                    && current.metadata.expires_at_unix_secs.saturating_sub(now)
                        > self.renew_before_seconds =>
            {
                (None, None)
            }
            Some(current)
                if current.metadata.credential_generation == credential_generation
                    && now < current.metadata.expires_at_unix_secs =>
            {
                let replacement = match self
                    .provider
                    .renew(reference, current.metadata(), now)
                    .await
                {
                    Ok(replacement) => replacement,
                    Err(error) => {
                        self.record_failure(
                            &subject,
                            Some(credential_generation),
                            audit_external_result(error),
                        )
                        .await;
                        return Err(map_external_error(error));
                    }
                };
                (Some(replacement), Some(CredentialAuditOperation::Renew))
            }
            current => {
                let replacement = match self
                    .provider
                    .acquire(reference, credential_generation, now)
                    .await
                {
                    Ok(replacement) => replacement,
                    Err(error) => {
                        self.record_failure(
                            &subject,
                            Some(credential_generation),
                            audit_external_result(error),
                        )
                        .await;
                        return Err(map_external_error(error));
                    }
                };
                let operation = match current {
                    None => CredentialAuditOperation::Create,
                    Some(current)
                        if current.metadata.credential_generation != credential_generation =>
                    {
                        CredentialAuditOperation::Rotate
                    }
                    Some(_) => CredentialAuditOperation::Renew,
                };
                (Some(replacement), Some(operation))
            }
        };

        if let Some(replacement) = replacement {
            if let Err(error) = validate_replacement(
                leases.get(binding_id),
                &replacement,
                credential_generation,
                now,
            ) {
                self.record_failure(
                    &subject,
                    Some(credential_generation),
                    CredentialAuditResult::InvalidLease,
                )
                .await;
                return Err(error);
            }
            let request = match replacement.request_lease(self.clock.clone()) {
                Ok(request) => request,
                Err(error) => {
                    self.record_failure(&subject, Some(credential_generation), audit_result(error))
                        .await;
                    return Err(error);
                }
            };
            self.audit
                .record(
                    &subject,
                    operation.expect("replacement operations are classified"),
                    Some(credential_generation),
                    CredentialAuditResult::Succeeded,
                )
                .await
                .map_err(|_| CredentialAccessError::RegistryUnavailable)?;
            leases.insert(binding_id.to_owned(), replacement);
            return Ok(request);
        }
        let request = match leases
            .get(binding_id)
            .ok_or(CredentialAccessError::Unavailable)?
            .request_lease(self.clock.clone())
        {
            Ok(request) => request,
            Err(error) => {
                self.record_failure(&subject, Some(credential_generation), audit_result(error))
                    .await;
                return Err(error);
            }
        };
        self.audit
            .record(
                &subject,
                CredentialAuditOperation::Renew,
                Some(credential_generation),
                CredentialAuditResult::Succeeded,
            )
            .await
            .map_err(|_| CredentialAccessError::RegistryUnavailable)?;
        Ok(request)
    }

    async fn record_failure(
        &self,
        subject: &CredentialAuditSubject,
        credential_revision: Option<u64>,
        result: CredentialAuditResult,
    ) {
        let _ = self
            .audit
            .record(
                subject,
                CredentialAuditOperation::Failure,
                credential_revision,
                result,
            )
            .await;
    }
}

impl std::fmt::Debug for RegistryCredentialSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegistryCredentialSource")
            .finish_non_exhaustive()
    }
}

impl ActiveCredentialSource for RegistryCredentialSource {
    fn resolve_active<'a>(
        &'a self,
        provider_id: &'a str,
        binding_id: &'a str,
    ) -> CredentialLeaseFuture<'a> {
        Box::pin(async move {
            self.resolve(provider_id, binding_id)
                .await
                .map(|lease| Box::new(lease) as Box<dyn ActiveCredentialLease>)
        })
    }
}

fn validate_replacement(
    previous: Option<&ExternalSecretLease>,
    replacement: &ExternalSecretLease,
    credential_generation: u64,
    now_unix_secs: i64,
) -> Result<(), CredentialAccessError> {
    let metadata = replacement.metadata();
    if metadata.credential_generation != credential_generation
        || metadata.issued_at_unix_secs != now_unix_secs
        || metadata.expires_at_unix_secs <= now_unix_secs
        || metadata.expires_at_unix_secs.saturating_sub(now_unix_secs)
            > MAX_EXTERNAL_SECRET_LEASE_SECONDS
        || previous.is_some_and(|current| {
            current.metadata.credential_generation == credential_generation
                && metadata.lease_generation <= current.metadata.lease_generation
        })
    {
        return Err(CredentialAccessError::Unavailable);
    }
    Ok(())
}

fn map_external_error(_: ExternalSecretLeaseError) -> CredentialAccessError {
    CredentialAccessError::Unavailable
}

const fn audit_external_result(error: ExternalSecretLeaseError) -> CredentialAuditResult {
    match error {
        ExternalSecretLeaseError::Unavailable => CredentialAuditResult::Unavailable,
        ExternalSecretLeaseError::InvalidLease => CredentialAuditResult::InvalidLease,
    }
}

const fn audit_result(error: CredentialAccessError) -> CredentialAuditResult {
    match error {
        CredentialAccessError::Unavailable => CredentialAuditResult::Unavailable,
        CredentialAccessError::RegistryUnavailable => CredentialAuditResult::RegistryUnavailable,
        CredentialAccessError::Integrity => CredentialAuditResult::Integrity,
        CredentialAccessError::InvalidEncoding => CredentialAuditResult::InvalidEncoding,
        CredentialAccessError::Expired => CredentialAuditResult::Expired,
    }
}

struct RequestCredentialLease {
    metadata: SecretLeaseMetadata,
    secret: ZeroingSecret,
    clock: Arc<dyn LeaseClock>,
}

impl ActiveCredentialLease for RequestCredentialLease {
    fn generation(&self) -> u64 {
        self.metadata.credential_generation
    }

    fn lease_generation(&self) -> u64 {
        self.metadata.lease_generation
    }

    fn expires_at_unix_secs(&self) -> i64 {
        self.metadata.expires_at_unix_secs
    }

    fn secret(&self) -> Result<&str, CredentialAccessError> {
        if self.clock.now_unix_secs() >= self.metadata.expires_at_unix_secs {
            return Err(CredentialAccessError::Expired);
        }
        self.secret
            .as_str()
            .map_err(|_| CredentialAccessError::InvalidEncoding)
    }
}

impl std::fmt::Debug for RequestCredentialLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RequestCredentialLease([REDACTED])")
    }
}

struct ZeroingSecret(Vec<u8>);

impl ZeroingSecret {
    fn new(bytes: Vec<u8>) -> Result<Self, ExternalSecretLeaseError> {
        if bytes.is_empty() {
            return Err(ExternalSecretLeaseError::InvalidLease);
        }
        Ok(Self(bytes))
    }

    fn as_str(&self) -> Result<&str, ExternalSecretLeaseError> {
        std::str::from_utf8(&self.0).map_err(|_| ExternalSecretLeaseError::InvalidLease)
    }

    fn duplicate(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Drop for ZeroingSecret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl std::fmt::Debug for ZeroingSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ZeroingSecret([REDACTED])")
    }
}

#[cfg(test)]
#[path = "../../tests/unit/credential_lease.rs"]
mod tests;
