//! Principal binding persistence and attack-surface tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use rusqlite::Connection;
use sylvander_protocol::UserId;
use tempfile::tempdir;

use crate::principal_binding::{
    Clock, ExternalPrincipal, PrincipalBindingError, PrincipalBindingStore, PrincipalDigestKey,
};

const DIGEST_KEY: &[u8] = b"principal-binding-test-key-32bytes";

fn digest_key() -> PrincipalDigestKey {
    PrincipalDigestKey::new(DIGEST_KEY).unwrap()
}

struct TestClock(AtomicI64);

impl TestClock {
    fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn principal(transport: &str, instance: &str, external: &str) -> ExternalPrincipal {
    ExternalPrincipal::new(transport, instance, external).unwrap()
}

async fn register(store: &PrincipalBindingStore, id: &str) -> UserId {
    let user = UserId::new(id);
    store.register_user(user.clone()).await.unwrap();
    user
}

async fn link(
    store: &PrincipalBindingStore,
    principal: ExternalPrincipal,
    user: UserId,
) -> crate::principal_binding::PrincipalBinding {
    let challenge = store
        .begin_link(principal.clone(), user, Duration::from_mins(1))
        .await
        .unwrap();
    store
        .confirm_link(
            principal,
            &challenge.challenge_id,
            challenge.secret.expose(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn binding_survives_restart_without_persisting_raw_external_id_or_secret() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("principals.db");
    let external = "telegram-user-raw-991";
    let secret;
    {
        let store = PrincipalBindingStore::open(&path, digest_key())
            .await
            .unwrap();
        let user = register(&store, "user-alice").await;
        let key = principal("telegram", "bot-primary", external);
        let challenge = store
            .begin_link(key.clone(), user.clone(), Duration::from_mins(1))
            .await
            .unwrap();
        secret = challenge.secret.expose().to_owned();
        store
            .confirm_link(key, &challenge.challenge_id, &secret)
            .await
            .unwrap();
    }

    let bytes = std::fs::read(&path).unwrap();
    assert!(
        !bytes
            .windows(external.len())
            .any(|window| window == external.as_bytes())
    );
    assert!(
        !bytes
            .windows(secret.len())
            .any(|window| window == secret.as_bytes())
    );

    let restarted = PrincipalBindingStore::open(&path, digest_key())
        .await
        .unwrap();
    let resolved = restarted
        .resolve(principal("telegram", "bot-primary", external))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.user_id, UserId::new("user-alice"));
    assert_eq!(resolved.revision, 1);
}

#[tokio::test]
async fn channel_instances_and_transports_are_distinct_identity_domains() {
    let store = PrincipalBindingStore::open_in_memory(Arc::new(TestClock::new(100)), digest_key())
        .await
        .unwrap();
    let alice = register(&store, "alice").await;
    let bob = register(&store, "bob").await;
    let carol = register(&store, "carol").await;

    link(
        &store,
        principal("telegram", "instance-a", "external-7"),
        alice.clone(),
    )
    .await;
    link(
        &store,
        principal("telegram", "instance-b", "external-7"),
        bob.clone(),
    )
    .await;
    link(
        &store,
        principal("dingtalk", "instance-a", "external-7"),
        carol.clone(),
    )
    .await;

    assert_eq!(
        store
            .resolve(principal("telegram", "instance-a", "external-7"))
            .await
            .unwrap()
            .unwrap()
            .user_id,
        alice
    );
    assert_eq!(
        store
            .resolve(principal("telegram", "instance-b", "external-7"))
            .await
            .unwrap()
            .unwrap()
            .user_id,
        bob
    );
    assert_eq!(
        store
            .resolve(principal("dingtalk", "instance-a", "external-7"))
            .await
            .unwrap()
            .unwrap()
            .user_id,
        carol
    );
}

#[tokio::test]
async fn linked_principal_cannot_be_silently_rebound_or_unlinked_by_another_user() {
    let store = PrincipalBindingStore::open_in_memory(Arc::new(TestClock::new(100)), digest_key())
        .await
        .unwrap();
    let alice = register(&store, "alice").await;
    let bob = register(&store, "bob").await;
    let key = principal("dingtalk", "corp-a", "staff-1");
    link(&store, key.clone(), alice.clone()).await;

    assert!(matches!(
        store
            .begin_link(key.clone(), bob.clone(), Duration::from_mins(1))
            .await,
        Err(PrincipalBindingError::AlreadyLinked)
    ));
    assert!(matches!(
        store.unlink(key.clone(), &bob, 1).await,
        Err(PrincipalBindingError::AlreadyLinked)
    ));
    assert!(matches!(
        store.unlink(key.clone(), &alice, 2).await,
        Err(PrincipalBindingError::Conflict {
            expected: 2,
            actual: 1
        })
    ));
    assert_eq!(store.resolve(key).await.unwrap().unwrap().user_id, alice);
}

#[tokio::test]
async fn unlink_and_relink_advance_revision_to_prevent_stale_cas_aba() {
    let store = PrincipalBindingStore::open_in_memory(Arc::new(TestClock::new(100)), digest_key())
        .await
        .unwrap();
    let alice = register(&store, "alice").await;
    let bob = register(&store, "bob").await;
    let key = principal("telegram", "bot-a", "external-a");
    let first = link(&store, key.clone(), alice.clone()).await;
    store
        .unlink(key.clone(), &alice, first.revision)
        .await
        .unwrap();
    let second = link(&store, key.clone(), bob.clone()).await;
    assert!(second.revision > first.revision);
    assert!(matches!(
        store.unlink(key.clone(), &bob, first.revision).await,
        Err(PrincipalBindingError::Conflict {
            expected: 1,
            actual: 3
        })
    ));
    assert_eq!(store.resolve(key).await.unwrap().unwrap().user_id, bob);
}

#[tokio::test]
async fn concurrent_confirmation_consumes_a_challenge_once() {
    let store = PrincipalBindingStore::open_in_memory(Arc::new(TestClock::new(100)), digest_key())
        .await
        .unwrap();
    let user = register(&store, "alice").await;
    let key = principal("telegram", "bot-a", "external-a");
    let challenge = store
        .begin_link(key.clone(), user, Duration::from_mins(1))
        .await
        .unwrap();
    let id = challenge.challenge_id;
    let secret = challenge.secret.expose().to_owned();
    let first = {
        let store = store.clone();
        let key = key.clone();
        let id = id.clone();
        let secret = secret.clone();
        tokio::spawn(async move { store.confirm_link(key, &id, &secret).await })
    };
    let second = {
        let store = store.clone();
        let key = key.clone();
        tokio::spawn(async move { store.confirm_link(key, &id, &secret).await })
    };
    let results = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(results.iter().any(|result| matches!(
        result,
        Err(PrincipalBindingError::UnknownChallenge | PrincipalBindingError::AlreadyLinked)
    )));
}

#[tokio::test]
async fn challenge_is_scoped_expires_and_has_a_bounded_guess_budget() {
    let clock = Arc::new(TestClock::new(100));
    let store = PrincipalBindingStore::open_in_memory(clock.clone(), digest_key())
        .await
        .unwrap();
    let user = register(&store, "alice").await;
    let key = principal("telegram", "bot-a", "external-a");
    let challenge = store
        .begin_link(key.clone(), user.clone(), Duration::from_secs(30))
        .await
        .unwrap();
    assert!(matches!(
        store
            .confirm_link(
                principal("telegram", "bot-b", "external-a"),
                &challenge.challenge_id,
                challenge.secret.expose()
            )
            .await,
        Err(PrincipalBindingError::ChallengePrincipalMismatch)
    ));
    clock.set(131);
    assert!(matches!(
        store
            .confirm_link(
                key.clone(),
                &challenge.challenge_id,
                challenge.secret.expose()
            )
            .await,
        Err(PrincipalBindingError::ChallengeExpired)
    ));

    clock.set(200);
    let challenge = store
        .begin_link(key.clone(), user, Duration::from_mins(1))
        .await
        .unwrap();
    for _ in 0..4 {
        assert!(matches!(
            store
                .confirm_link(key.clone(), &challenge.challenge_id, "wrong")
                .await,
            Err(PrincipalBindingError::InvalidChallengeSecret)
        ));
    }
    assert!(matches!(
        store
            .confirm_link(key.clone(), &challenge.challenge_id, "wrong")
            .await,
        Err(PrincipalBindingError::ChallengeLocked)
    ));
    assert!(matches!(
        store
            .confirm_link(key, &challenge.challenge_id, challenge.secret.expose())
            .await,
        Err(PrincipalBindingError::UnknownChallenge)
    ));
}

#[tokio::test]
async fn latest_challenge_invalidates_older_secret_and_unknown_users_fail_closed() {
    let store = PrincipalBindingStore::open_in_memory(Arc::new(TestClock::new(100)), digest_key())
        .await
        .unwrap();
    let key = principal("telegram", "bot-a", "external-a");
    assert!(matches!(
        store
            .begin_link(
                key.clone(),
                UserId::new("missing"),
                Duration::from_mins(1)
            )
            .await,
        Err(PrincipalBindingError::UnknownUser(user)) if user == "missing"
    ));
    let user = register(&store, "alice").await;
    let old = store
        .begin_link(key.clone(), user.clone(), Duration::from_mins(1))
        .await
        .unwrap();
    let current = store
        .begin_link(key.clone(), user, Duration::from_mins(1))
        .await
        .unwrap();
    assert!(matches!(
        store
            .confirm_link(key.clone(), &old.challenge_id, old.secret.expose())
            .await,
        Err(PrincipalBindingError::UnknownChallenge)
    ));
    store
        .confirm_link(key, &current.challenge_id, current.secret.expose())
        .await
        .unwrap();
}

#[tokio::test]
async fn incompatible_existing_schema_is_rejected_instead_of_migrated() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("old.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE legacy(value TEXT); PRAGMA user_version=0;")
        .unwrap();
    drop(connection);

    assert!(matches!(
        PrincipalBindingStore::open(path, digest_key()).await,
        Err(PrincipalBindingError::IncompatibleSchema)
    ));
}

#[tokio::test]
async fn wrong_digest_key_and_extra_schema_objects_fail_closed() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("principals.db");
    PrincipalBindingStore::open(&path, digest_key())
        .await
        .unwrap();

    assert!(matches!(
        PrincipalBindingStore::open(
            &path,
            PrincipalDigestKey::new(b"different-principal-binding-key-32").unwrap()
        )
        .await,
        Err(PrincipalBindingError::Storage)
    ));

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE injected(value TEXT) STRICT;")
        .unwrap();
    drop(connection);
    assert!(matches!(
        PrincipalBindingStore::open(&path, digest_key()).await,
        Err(PrincipalBindingError::IncompatibleSchema)
    ));
}
