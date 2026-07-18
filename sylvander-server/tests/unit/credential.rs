use std::sync::Arc;

use sylvander_channel::credential::{
    CredentialLeaseError, CredentialLeaseRequest, CredentialLeaseSource,
};
use sylvander_runtime::config::{SecretRef, SystemSecretResolver};
use sylvander_runtime::credential_audit::{
    CredentialAuditOperation, CredentialAuditResult, CredentialAuditSubject,
    CredentialOperationAuditLedger,
};
use tempfile::tempdir;

use super::*;

fn reference(path: &std::path::Path) -> SecretRef {
    SecretRef::File {
        path: path.to_path_buf(),
    }
}

#[tokio::test]
async fn resolves_atomic_bundle_and_observes_rotation_without_restart() {
    let directory = tempdir().unwrap();
    let token = directory.path().join("token");
    let webhook = directory.path().join("webhook");
    std::fs::write(&token, "token-one\n").unwrap();
    std::fs::write(&webhook, "webhook-one\n").unwrap();
    let audit_path = directory.path().join("credential-audit.db");
    let audit = Arc::new(
        CredentialOperationAuditLedger::open(&audit_path)
            .await
            .unwrap(),
    );
    let source = SystemChannelCredentialSource::new(
        "telegram-primary",
        [
            ("bot_token".into(), reference(&token)),
            ("webhook_secret".into(), reference(&webhook)),
        ],
        Arc::new(SystemSecretResolver),
        audit.clone(),
    )
    .unwrap();
    let request =
        CredentialLeaseRequest::new("telegram-primary", ["bot_token", "webhook_secret"]).unwrap();

    let first = source.lease(&request).await.unwrap();
    assert_eq!(first.credential_generation(), 1);
    assert_eq!(first.lease_generation(), 1);
    assert_eq!(first.secret("bot_token").unwrap(), "token-one");
    assert_eq!(first.secret("webhook_secret").unwrap(), "webhook-one");

    std::fs::write(&token, "token-two\n").unwrap();
    let rotated = source.lease(&request).await.unwrap();
    assert_eq!(rotated.credential_generation(), 2);
    assert_eq!(rotated.lease_generation(), 2);
    assert_eq!(rotated.secret("bot_token").unwrap(), "token-two");
    assert_eq!(rotated.secret("webhook_secret").unwrap(), "webhook-one");
    assert!(matches!(
        rotated.secret_at("bot_token", rotated.expires_at_unix_secs()),
        Err(CredentialLeaseError::Expired)
    ));
    let subject = CredentialAuditSubject::channel_instance("telegram-primary").unwrap();
    let events = audit.list(&subject, 10).await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].operation, CredentialAuditOperation::Rotate);
    assert_eq!(events[1].operation, CredentialAuditOperation::Create);
    let persisted = std::fs::read(audit_path).unwrap();
    for secret in ["token-one", "token-two", "webhook-one"] {
        assert!(
            !persisted
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
        );
    }
}

#[tokio::test]
async fn partial_resolution_failure_never_publishes_or_advances_a_bundle() {
    let directory = tempdir().unwrap();
    let token = directory.path().join("token");
    let webhook = directory.path().join("webhook");
    std::fs::write(&token, "token-one").unwrap();
    std::fs::write(&webhook, "webhook-one").unwrap();
    let audit = Arc::new(
        CredentialOperationAuditLedger::open(directory.path().join("credential-audit.db"))
            .await
            .unwrap(),
    );
    let source = SystemChannelCredentialSource::new(
        "telegram-primary",
        [
            ("bot_token".into(), reference(&token)),
            ("webhook_secret".into(), reference(&webhook)),
        ],
        Arc::new(SystemSecretResolver),
        audit.clone(),
    )
    .unwrap();
    let request =
        CredentialLeaseRequest::new("telegram-primary", ["bot_token", "webhook_secret"]).unwrap();
    assert_eq!(
        source
            .lease(&request)
            .await
            .unwrap()
            .credential_generation(),
        1
    );

    std::fs::remove_file(&webhook).unwrap();
    std::fs::write(&token, "token-two").unwrap();
    assert!(matches!(
        source.lease(&request).await,
        Err(CredentialLeaseError::Unavailable)
    ));

    std::fs::write(&webhook, "webhook-two").unwrap();
    let recovered = source.lease(&request).await.unwrap();
    assert_eq!(recovered.credential_generation(), 2);
    assert_eq!(recovered.lease_generation(), 2);
    assert_eq!(
        recovered.secret("token-does-not-exist"),
        Err(CredentialLeaseError::MissingSlot)
    );
    let subject = CredentialAuditSubject::channel_instance("telegram-primary").unwrap();
    let events = audit.list(&subject, 10).await.unwrap();
    assert!(events.iter().any(|event| {
        event.operation == CredentialAuditOperation::Failure
            && event.result == CredentialAuditResult::Unavailable
    }));
}

#[tokio::test]
async fn instance_and_slot_boundaries_fail_closed_and_debug_is_redacted() {
    let directory = tempdir().unwrap();
    let secret = directory.path().join("secret-locator");
    std::fs::write(&secret, "do-not-print").unwrap();
    let audit = Arc::new(
        CredentialOperationAuditLedger::open(directory.path().join("credential-audit.db"))
            .await
            .unwrap(),
    );
    let source = SystemChannelCredentialSource::new(
        "http-primary",
        [("bearer_token".into(), reference(&secret))],
        Arc::new(SystemSecretResolver),
        audit.clone(),
    )
    .unwrap();

    let wrong_instance = CredentialLeaseRequest::new("http-secondary", ["bearer_token"]).unwrap();
    assert!(matches!(
        source.lease(&wrong_instance).await,
        Err(CredentialLeaseError::Unavailable)
    ));
    let unknown_slot = CredentialLeaseRequest::new("http-primary", ["other_token"]).unwrap();
    assert!(matches!(
        source.lease(&unknown_slot).await,
        Err(CredentialLeaseError::Unavailable)
    ));

    let debug = format!("{source:?}");
    assert!(!debug.contains("do-not-print"));
    assert!(!debug.contains("secret-locator"));
    assert!(debug.contains("[REDACTED]"));
    let primary = CredentialAuditSubject::channel_instance("http-primary").unwrap();
    let secondary = CredentialAuditSubject::channel_instance("http-secondary").unwrap();
    assert_eq!(audit.list(&primary, 10).await.unwrap().len(), 2);
    assert!(audit.list(&secondary, 10).await.unwrap().is_empty());
}
