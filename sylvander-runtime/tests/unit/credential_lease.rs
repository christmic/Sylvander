use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicI64, Ordering};

use tempfile::tempdir;

use super::*;
use crate::registry_domain::CredentialBindingRevision;

const BINDING: &str = "provider:anthropic:api_key";

#[derive(Default)]
struct TestClock(AtomicI64);

impl TestClock {
    fn at(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl LeaseClock for TestClock {
    fn now_unix_secs(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

enum Reply {
    Lease {
        lease_generation: u64,
        ttl: i64,
        value: &'static str,
    },
    WrongCredentialGeneration {
        lease_generation: u64,
    },
    Error,
}

#[derive(Default)]
struct ProviderCalls {
    acquire: usize,
    renew: usize,
}

struct TestProvider {
    acquire: StdMutex<VecDeque<Reply>>,
    renew: StdMutex<VecDeque<Reply>>,
    calls: StdMutex<ProviderCalls>,
}

impl TestProvider {
    fn new(
        acquire: impl IntoIterator<Item = Reply>,
        renew: impl IntoIterator<Item = Reply>,
    ) -> Self {
        Self {
            acquire: StdMutex::new(acquire.into_iter().collect()),
            renew: StdMutex::new(renew.into_iter().collect()),
            calls: StdMutex::new(ProviderCalls::default()),
        }
    }

    fn reply(
        reply: Reply,
        credential_generation: u64,
        now: i64,
    ) -> Result<ExternalSecretLease, ExternalSecretLeaseError> {
        match reply {
            Reply::Lease {
                lease_generation,
                ttl,
                value,
            } => ExternalSecretLease::new(
                credential_generation,
                lease_generation,
                now,
                now + ttl,
                value.as_bytes().to_vec(),
            ),
            Reply::WrongCredentialGeneration { lease_generation } => ExternalSecretLease::new(
                credential_generation + 1,
                lease_generation,
                now,
                now + 10,
                b"wrong-generation".to_vec(),
            ),
            Reply::Error => Err(ExternalSecretLeaseError::Unavailable),
        }
    }
}

impl RenewableExternalSecretProvider for TestProvider {
    fn acquire<'a>(
        &'a self,
        _reference: &'a SecretRef,
        credential_generation: u64,
        now_unix_secs: i64,
    ) -> ExternalSecretLeaseFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().acquire += 1;
            let reply = self.acquire.lock().unwrap().pop_front().unwrap();
            Self::reply(reply, credential_generation, now_unix_secs)
        })
    }

    fn renew<'a>(
        &'a self,
        _reference: &'a SecretRef,
        current: SecretLeaseMetadata,
        now_unix_secs: i64,
    ) -> ExternalSecretLeaseFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().renew += 1;
            let reply = self.renew.lock().unwrap().pop_front().unwrap();
            Self::reply(reply, current.credential_generation, now_unix_secs)
        })
    }
}

async fn registry_with_generation_one() -> (tempfile::TempDir, AgentRegistry) {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    registry
        .seed_credential(CredentialBindingRevision {
            binding_id: BINDING.into(),
            generation: 1,
            reference: SecretRef::File {
                path: PathBuf::from("/external/provider/reference"),
            },
        })
        .await
        .unwrap();
    (directory, registry)
}

async fn source(
    registry: AgentRegistry,
    provider: Arc<TestProvider>,
    clock: Arc<TestClock>,
    renew_before: i64,
) -> RegistryCredentialSource {
    let audit = Arc::new(
        CredentialOperationAuditLedger::open_in_memory_with_policy(1_000, 100)
            .await
            .unwrap(),
    );
    RegistryCredentialSource::with_provider(registry, provider, clock, renew_before, audit).unwrap()
}

#[tokio::test]
async fn live_lease_is_reused_then_renewed_before_expiry() {
    let (_directory, registry) = registry_with_generation_one().await;
    let provider = Arc::new(TestProvider::new(
        [Reply::Lease {
            lease_generation: 1,
            ttl: 20,
            value: "first",
        }],
        [Reply::Lease {
            lease_generation: 2,
            ttl: 20,
            value: "renewed",
        }],
    ));
    let clock = Arc::new(TestClock::at(100));
    let source = source(registry, provider.clone(), clock.clone(), 5).await;

    let first = source.resolve_active("anthropic", BINDING).await.unwrap();
    assert_eq!(first.generation(), 1);
    assert_eq!(first.lease_generation(), 1);
    assert_eq!(first.expires_at_unix_secs(), 120);
    assert_eq!(first.secret().unwrap(), "first");

    clock.set(110);
    let reused = source.resolve_active("anthropic", BINDING).await.unwrap();
    assert_eq!(reused.lease_generation(), 1);
    assert_eq!(reused.secret().unwrap(), "first");

    clock.set(116);
    let renewed = source.resolve_active("anthropic", BINDING).await.unwrap();
    assert_eq!(renewed.lease_generation(), 2);
    assert_eq!(renewed.expires_at_unix_secs(), 136);
    assert_eq!(renewed.secret().unwrap(), "renewed");
    {
        let calls = provider.calls.lock().unwrap();
        assert_eq!(calls.acquire, 1);
        assert_eq!(calls.renew, 1);
    }
    let subject = CredentialAuditSubject::provider("anthropic", BINDING).unwrap();
    let events = source.audit.list(&subject, 10).await.unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].operation, CredentialAuditOperation::Renew);
    assert_eq!(events[1].operation, CredentialAuditOperation::Renew);
    assert_eq!(events[2].operation, CredentialAuditOperation::Create);
}

#[tokio::test]
async fn renewal_failure_never_falls_back_to_still_live_cached_bytes() {
    let (_directory, registry) = registry_with_generation_one().await;
    let provider = Arc::new(TestProvider::new(
        [Reply::Lease {
            lease_generation: 1,
            ttl: 10,
            value: "must-not-fallback",
        }],
        [Reply::Error],
    ));
    let clock = Arc::new(TestClock::at(200));
    let source = source(registry, provider, clock.clone(), 3).await;
    source.resolve_active("anthropic", BINDING).await.unwrap();

    clock.set(208);
    let result = source.resolve_active("anthropic", BINDING).await;

    assert!(matches!(result, Err(CredentialAccessError::Unavailable)));
    let subject = CredentialAuditSubject::provider("anthropic", BINDING).unwrap();
    let events = source.audit.list(&subject, 10).await.unwrap();
    assert_eq!(events[0].operation, CredentialAuditOperation::Failure);
    assert_eq!(events[0].result, CredentialAuditResult::Unavailable);
}

#[tokio::test]
async fn expired_request_lease_fails_closed_after_it_was_issued() {
    let (_directory, registry) = registry_with_generation_one().await;
    let provider = Arc::new(TestProvider::new(
        [Reply::Lease {
            lease_generation: 1,
            ttl: 4,
            value: "short-lived",
        }],
        [],
    ));
    let clock = Arc::new(TestClock::at(300));
    let source = source(registry, provider, clock.clone(), 0).await;
    let lease = source.resolve_active("anthropic", BINDING).await.unwrap();
    assert_eq!(lease.secret().unwrap(), "short-lived");

    clock.set(304);

    assert!(matches!(
        lease.secret(),
        Err(CredentialAccessError::Expired)
    ));
}

#[tokio::test]
async fn expired_cache_is_never_used_when_reacquisition_fails() {
    let (_directory, registry) = registry_with_generation_one().await;
    let provider = Arc::new(TestProvider::new(
        [
            Reply::Lease {
                lease_generation: 1,
                ttl: 4,
                value: "expired",
            },
            Reply::Error,
        ],
        [],
    ));
    let clock = Arc::new(TestClock::at(400));
    let source = source(registry, provider.clone(), clock.clone(), 0).await;
    source.resolve_active("anthropic", BINDING).await.unwrap();

    clock.set(405);
    let result = source.resolve_active("anthropic", BINDING).await;

    assert!(matches!(result, Err(CredentialAccessError::Unavailable)));
    let calls = provider.calls.lock().unwrap();
    assert_eq!(calls.acquire, 2);
    assert_eq!(calls.renew, 0);
}

#[tokio::test]
async fn registry_generation_rotation_invalidates_a_live_lease() {
    let (_directory, registry) = registry_with_generation_one().await;
    let provider = Arc::new(TestProvider::new(
        [
            Reply::Lease {
                lease_generation: 1,
                ttl: 30,
                value: "generation-one",
            },
            Reply::Lease {
                lease_generation: 2,
                ttl: 30,
                value: "generation-two",
            },
        ],
        [],
    ));
    let clock = Arc::new(TestClock::at(500));
    let source = source(registry.clone(), provider.clone(), clock, 5).await;
    let first = source.resolve_active("anthropic", BINDING).await.unwrap();
    assert_eq!(first.secret().unwrap(), "generation-one");

    registry
        .stage_credential(
            1,
            CredentialBindingRevision {
                binding_id: BINDING.into(),
                generation: 2,
                reference: SecretRef::File {
                    path: PathBuf::from("/external/provider/rotated-reference"),
                },
            },
        )
        .await
        .unwrap();
    registry.activate_credential(BINDING, 2, 1).await.unwrap();

    let rotated = source.resolve_active("anthropic", BINDING).await.unwrap();
    assert_eq!(rotated.generation(), 2);
    assert_eq!(rotated.lease_generation(), 2);
    assert_eq!(rotated.secret().unwrap(), "generation-two");
    assert_eq!(provider.calls.lock().unwrap().acquire, 2);
    let subject = CredentialAuditSubject::provider("anthropic", BINDING).unwrap();
    let events = source.audit.list(&subject, 10).await.unwrap();
    assert_eq!(events[0].operation, CredentialAuditOperation::Rotate);
    assert_eq!(events[0].credential_revision, Some(2));
}

#[tokio::test]
async fn malformed_external_generations_are_rejected_and_not_cached() {
    let (_directory, registry) = registry_with_generation_one().await;
    let provider = Arc::new(TestProvider::new(
        [
            Reply::WrongCredentialGeneration {
                lease_generation: 1,
            },
            Reply::Lease {
                lease_generation: 2,
                ttl: 10,
                value: "valid",
            },
        ],
        [],
    ));
    let clock = Arc::new(TestClock::at(600));
    let source = source(registry, provider.clone(), clock, 1).await;

    assert!(matches!(
        source.resolve_active("anthropic", BINDING).await,
        Err(CredentialAccessError::Unavailable)
    ));
    let valid = source.resolve_active("anthropic", BINDING).await.unwrap();
    assert_eq!(valid.secret().unwrap(), "valid");
    assert_eq!(provider.calls.lock().unwrap().acquire, 2);
}

#[test]
fn lease_debug_output_is_redacted_and_ttl_is_bounded() {
    let lease = ExternalSecretLease::new(1, 7, 700, 710, b"do-not-print".to_vec()).unwrap();
    let debug = format!("{lease:?}");
    assert_eq!(debug, "ExternalSecretLease([REDACTED])");
    assert!(!debug.contains("do-not-print"));
    assert!(matches!(
        ExternalSecretLease::new(1, 8, 700, 1_001, b"secret".to_vec()),
        Err(ExternalSecretLeaseError::InvalidLease)
    ));
}
