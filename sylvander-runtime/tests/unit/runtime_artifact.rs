use sylvander_agent::artifact::{ArtifactStoreError, ArtifactWrite};

use super::*;
use crate::agent::cognition_artifact::{CognitionArtifactError, CognitionArtifactKind};
use crate::evidence::{EvidenceEncryption, EvidenceGovernance, EvidenceScope};
use crate::storage::session::PerceptionInvocationId;

fn governance() -> EvidenceGovernance {
    let encryption = EvidenceEncryption::from_secret("test-key", &[7; 32]).unwrap();
    EvidenceGovernance::new("tenant-a", 30, encryption).unwrap()
}

fn binding(user_id: &str) -> ArtifactTurnBinding {
    ArtifactTurnBinding {
        user_id: user_id.to_string(),
        agent_id: "agent-a".to_string(),
        session_id: "session-a".to_string(),
        turn_id: "turn-a".to_string(),
        created_at: crate::session::now_secs(),
    }
}

#[tokio::test]
async fn governed_artifact_is_encrypted_scoped_and_location_neutral() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("artifacts.sqlite");
    let store = EvidenceStore::open_governed(&path, governance())
        .await
        .unwrap();
    let service = RuntimeArtifactService::new(store.clone()).unwrap();
    let port = service.bind(binding("alice")).unwrap();
    let payload = b"PRIVATE-TOOL-OUTPUT".to_vec();

    let reference = port
        .persist(ArtifactWrite {
            call_id: "call/with/untrusted/path".to_string(),
            media_type: "text/plain; charset=utf-8".to_string(),
            payload: payload.clone(),
        })
        .await
        .unwrap();

    assert!(reference.locator.starts_with("artifact:"));
    assert!(!reference.locator.contains("alice") && !reference.locator.contains('/'));
    assert_eq!(reference.original_bytes, payload.len());
    let record_id = reference.locator.strip_prefix("artifact:").unwrap();
    let export = store
        .export_governed_records(
            EvidenceScope::new("tenant-a", "alice"),
            vec![record_id.to_string()],
            crate::session::now_secs(),
        )
        .await
        .unwrap();
    assert_eq!(export.records[0].payload, payload);
    assert!(
        export.records[0]
            .source_ref
            .starts_with("artifact-session-sha256:")
    );
    drop(port);
    drop(service);
    drop(store);
    let database = std::fs::read(path).unwrap();
    assert!(
        !database
            .windows(payload.len())
            .any(|window| window == payload)
    );
}

#[tokio::test]
async fn range_read_is_bounded_session_scoped_and_audited() {
    let store = EvidenceStore::open_governed_in_memory(governance())
        .await
        .unwrap();
    let service = RuntimeArtifactService::new(store.clone()).unwrap();
    let port = service.bind(binding("alice")).unwrap();
    let payload = vec![b'x'; sylvander_api::MAX_ARTIFACT_READ_BYTES + 17];
    let reference = port
        .persist(ArtifactWrite {
            call_id: "call-a".into(),
            media_type: "application/octet-stream".into(),
            payload: payload.clone(),
        })
        .await
        .unwrap();
    let now = crate::session::now_secs();

    let first = service
        .read_range(
            "alice".into(),
            "session-a".into(),
            &reference.locator,
            0,
            sylvander_api::MAX_ARTIFACT_READ_BYTES,
            now,
        )
        .await
        .unwrap();
    assert_eq!(first.bytes.len(), sylvander_api::MAX_ARTIFACT_READ_BYTES);
    assert_eq!(first.total_bytes, payload.len());
    let second = service
        .read_range(
            "alice".into(),
            "session-a".into(),
            &reference.locator,
            first.bytes.len(),
            sylvander_api::MAX_ARTIFACT_READ_BYTES,
            now,
        )
        .await
        .unwrap();
    assert_eq!(second.bytes, vec![b'x'; 17]);
    assert_eq!(first.payload_digest_sha256, second.payload_digest_sha256);

    for (user, session) in [("bob", "session-a"), ("alice", "session-b")] {
        let denied = service
            .read_range(user.into(), session.into(), &reference.locator, 0, 1, now)
            .await;
        assert!(
            matches!(denied, Err(EvidenceError::GovernedRecordNotFound)),
            "unexpected range denial for {user}/{session}: {denied:?}"
        );
    }
    let audits = store
        .governance_audits(EvidenceScope::new("tenant-a", "alice"), 10)
        .await
        .unwrap();
    assert_eq!(audits.len(), 2);
    assert!(audits.iter().all(|audit| audit.action == "read"));
}

#[tokio::test]
async fn range_read_rejects_invalid_locator_offset_expiry_and_deleted_record() {
    let store = EvidenceStore::open_governed_in_memory(governance())
        .await
        .unwrap();
    let service = RuntimeArtifactService::new(store.clone()).unwrap();
    let port = service.bind(binding("alice")).unwrap();
    let reference = port
        .persist(ArtifactWrite {
            call_id: "call-a".into(),
            media_type: "text/plain".into(),
            payload: b"artifact".to_vec(),
        })
        .await
        .unwrap();
    let now = crate::session::now_secs();

    for (locator, offset, read_at) in [
        ("file:///private/path", 0, now),
        (reference.locator.as_str(), 9, now),
        (reference.locator.as_str(), 0, now + 31 * 86_400),
    ] {
        let denied = service
            .read_range(
                "alice".into(),
                "session-a".into(),
                locator,
                offset,
                1,
                read_at,
            )
            .await;
        assert!(
            matches!(denied, Err(EvidenceError::GovernedRecordNotFound)),
            "unexpected range denial for {locator}: {denied:?}"
        );
    }

    let record_id = reference.locator.strip_prefix("artifact:").unwrap();
    store
        .delete_governed_records(
            EvidenceScope::new("tenant-a", "alice"),
            vec![record_id.into()],
            "user deletion".into(),
            now,
        )
        .await
        .unwrap();
    let deleted = service
        .read_range(
            "alice".into(),
            "session-a".into(),
            &reference.locator,
            0,
            1,
            now,
        )
        .await;
    assert!(matches!(
        deleted,
        Err(EvidenceError::GovernedRecordNotFound)
    ));
}

#[tokio::test]
async fn plaintext_backend_and_invalid_payload_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let plaintext = EvidenceStore::open(directory.path().join("plaintext.sqlite"))
        .await
        .unwrap();
    assert!(matches!(
        RuntimeArtifactService::new(plaintext),
        Err(EvidenceError::EncryptionRequired)
    ));

    let store = EvidenceStore::open_governed_in_memory(governance())
        .await
        .unwrap();
    let port = RuntimeArtifactService::new(store)
        .unwrap()
        .bind(binding("alice"))
        .unwrap();
    let error = port
        .persist(ArtifactWrite {
            call_id: "call".to_string(),
            media_type: "text/plain".to_string(),
            payload: Vec::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(error, ArtifactStoreError::InvalidRequest);
}

#[tokio::test]
async fn exact_perception_artifact_is_idempotent_conflict_safe_and_restart_readable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("perception.sqlite");
    let invocation_id =
        PerceptionInvocationId::parse("0198ae9d-7c42-7821-a924-201733a5a7cc").unwrap();
    let payload = b"governed-media".to_vec();
    let store = EvidenceStore::open_governed(&path, governance())
        .await
        .unwrap();
    let service = RuntimeArtifactService::new(store).unwrap();
    let port = service.bind_cognition(binding("alice")).unwrap();

    let first = port
        .persist_exact(
            invocation_id.as_str(),
            CognitionArtifactKind::SourceMedia,
            "audio/wav",
            payload.clone(),
        )
        .await
        .unwrap();
    let repeated = port
        .persist_exact(
            invocation_id.as_str(),
            CognitionArtifactKind::SourceMedia,
            "audio/wav",
            payload.clone(),
        )
        .await
        .unwrap();
    assert_eq!(first, repeated);
    assert_eq!(
        port.persist_exact(
            invocation_id.as_str(),
            CognitionArtifactKind::SourceMedia,
            "audio/wav",
            b"different".to_vec(),
        )
        .await,
        Err(CognitionArtifactError::Conflict)
    );
    drop(port);
    drop(service);

    let reopened = EvidenceStore::open_governed(&path, governance())
        .await
        .unwrap();
    let recovered = RuntimeArtifactService::new(reopened)
        .unwrap()
        .bind_cognition(binding("alice"))
        .unwrap()
        .load_exact(invocation_id.as_str(), CognitionArtifactKind::SourceMedia)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered, first);
}
