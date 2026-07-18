use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use rusqlite::Connection;
use sylvander_protocol::{
    AccessibilityPreferences, ClassifiedPreference, CommunicationTone, LanguageTag, PrivacyClass,
    ProfileConstraint, ResponseDetail,
};
use tempfile::tempdir;

use super::*;

struct TestClock(AtomicI64);

impl TestClock {
    fn new(now: i64) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(now)))
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

fn owner(value: &str) -> UserId {
    UserId::new(value)
}

fn profile(language: &str, constraint: &str) -> UserProfileData {
    UserProfileData {
        preferred_language: Some(ClassifiedPreference {
            value: LanguageTag::new(language).unwrap(),
            privacy_class: PrivacyClass::Personal,
        }),
        response_detail: Some(ClassifiedPreference {
            value: ResponseDetail::Detailed,
            privacy_class: PrivacyClass::Personal,
        }),
        communication_tone: Some(ClassifiedPreference {
            value: CommunicationTone::Warm,
            privacy_class: PrivacyClass::Sensitive,
        }),
        accessibility: Some(ClassifiedPreference {
            value: AccessibilityPreferences {
                screen_reader_optimized: true,
                reduce_motion: false,
                high_contrast: true,
            },
            privacy_class: PrivacyClass::Restricted,
        }),
        constraints: vec![ClassifiedPreference {
            value: ProfileConstraint::new(constraint).unwrap(),
            privacy_class: PrivacyClass::Restricted,
        }],
        ..UserProfileData::default()
    }
}

#[tokio::test]
async fn typed_lifecycle_uses_monotonic_cas_and_preserves_delete_tombstone() {
    let clock = TestClock::new(100);
    let store = UserProfileStore::open_in_memory(clock.clone())
        .await
        .unwrap();
    let alice = owner("alice");
    let original = profile("zh-CN", "never reveal private notes");

    let created = store.create(alice.clone(), original.clone()).await.unwrap();
    assert_eq!(created.revision, 1);
    assert_eq!(created.profile, original);
    assert_eq!(created.created_at_unix_secs, 100);
    assert_eq!(
        store
            .create(alice.clone(), UserProfileData::default())
            .await,
        Err(UserProfileStoreError::AlreadyExists)
    );

    clock.set(110);
    let updated = store
        .update(alice.clone(), 1, profile("en-GB", "prefer examples"))
        .await
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(
        store.update(alice.clone(), 1, original.clone()).await,
        Err(UserProfileStoreError::Conflict {
            expected: 1,
            actual: 2
        })
    );
    let corrected = store.correct(alice.clone(), 2, original).await.unwrap();
    let blocked = store
        .set_do_not_learn(alice.clone(), corrected.revision, true)
        .await
        .unwrap();
    assert!(blocked.do_not_learn);

    clock.set(120);
    let export = store.export(alice.clone()).await.unwrap();
    assert_eq!(export.profile.revision, 4);
    assert_eq!(export.exported_at_unix_secs, 120);
    assert!(!serde_json::to_string(&export).unwrap().contains("alice"));

    let tombstone_revision = store.delete(alice.clone(), 4).await.unwrap();
    assert_eq!(tombstone_revision, 5);
    assert_eq!(store.read(alice.clone()).await.unwrap(), None);
    assert_eq!(
        store.export(alice.clone()).await,
        Err(UserProfileStoreError::NotFound)
    );
    let recreated = store
        .create(alice, UserProfileData::default())
        .await
        .unwrap();
    assert_eq!(recreated.revision, 6);
    assert!(recreated.do_not_learn);
}

#[tokio::test]
async fn restart_restores_exact_owner_profile_and_isolates_other_users() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("profiles.sqlite3");
    let alice = owner("alice");
    let bob = owner("bob");
    let first = UserProfileStore::open(&path).await.unwrap();
    first
        .create(alice.clone(), profile("zh-CN", "private constraint"))
        .await
        .unwrap();
    drop(first);

    let reopened = UserProfileStore::open(&path).await.unwrap();
    assert_eq!(reopened.read(alice).await.unwrap().unwrap().revision, 1);
    assert_eq!(reopened.read(bob).await.unwrap(), None);
}

#[tokio::test]
async fn concurrent_stores_allow_exactly_one_update_for_a_revision() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("profiles.sqlite3");
    let first = UserProfileStore::open(&path).await.unwrap();
    let second = UserProfileStore::open(&path).await.unwrap();
    first
        .create(owner("alice"), UserProfileData::default())
        .await
        .unwrap();

    let (left, right) = tokio::join!(
        first.update(owner("alice"), 1, profile("en", "left")),
        second.update(owner("alice"), 1, profile("fr", "right"))
    );
    let results = [left, right];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(results.iter().any(|result| {
        matches!(
            result,
            Err(UserProfileStoreError::Conflict {
                expected: 1,
                actual: 2
            })
        )
    }));
}

#[tokio::test]
async fn rejects_oversized_payload_unknown_schema_and_corrupt_content_safely() {
    let clock = TestClock::new(100);
    let store = UserProfileStore::open_in_memory(clock).await.unwrap();
    let constraint = ClassifiedPreference {
        value: ProfileConstraint::new("secret-content").unwrap(),
        privacy_class: PrivacyClass::Restricted,
    };
    let oversized = UserProfileData {
        constraints: vec![constraint; 17],
        ..UserProfileData::default()
    };
    assert_eq!(
        store.create(owner("alice"), oversized).await,
        Err(UserProfileStoreError::Invalid("constraints"))
    );
    store
        .create(owner("alice"), profile("en", "secret-content"))
        .await
        .unwrap();
    store
        .run(|connection| {
            connection
                .execute(
                    "UPDATE user_profiles SET profile_json='{\"unknown\":\"secret-content\"}' WHERE user_id='alice'",
                    [],
                )
                .map_err(storage)?;
            Ok(())
        })
        .await
        .unwrap();
    let error = store.read(owner("alice")).await.unwrap_err();
    assert_eq!(error, UserProfileStoreError::Corrupt);
    assert!(!format!("{error:?}").contains("secret-content"));

    let directory = tempdir().unwrap();
    let path = directory.path().join("wrong.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(&format!(
            "PRAGMA application_id={APPLICATION_ID}; PRAGMA user_version={SCHEMA_VERSION}; CREATE TABLE wrong(value TEXT) STRICT;"
        ))
        .unwrap();
    drop(connection);
    assert!(matches!(
        UserProfileStore::open(path).await,
        Err(UserProfileStoreError::IncompatibleSchema)
    ));
}
