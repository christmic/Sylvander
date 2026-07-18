use super::*;

#[test]
fn request_canonicalizes_slots_and_rejects_duplicates() {
    let request =
        CredentialLeaseRequest::new("telegram-primary", ["webhook_secret", "bot_token"]).unwrap();
    assert_eq!(request.slots, ["bot_token", "webhook_secret"]);
    assert!(matches!(
        CredentialLeaseRequest::new("telegram-primary", ["bot_token", "bot_token"]),
        Err(CredentialLeaseError::InvalidRequest)
    ));
}

#[test]
fn bundle_is_atomic_redacted_and_expires_fail_closed() {
    let bundle = CredentialLeaseBundle::new(
        4,
        9,
        100,
        110,
        [
            ("bot_token".into(), b"bot-secret".to_vec()),
            ("webhook_secret".into(), b"webhook-secret".to_vec()),
        ],
    )
    .unwrap();
    assert_eq!(bundle.credential_generation(), 4);
    assert_eq!(bundle.lease_generation(), 9);
    assert_eq!(bundle.secret_at("bot_token", 109).unwrap(), "bot-secret");
    assert!(matches!(
        bundle.secret_at("bot_token", 110),
        Err(CredentialLeaseError::Expired)
    ));
    assert!(matches!(
        bundle.secret_at("missing", 109),
        Err(CredentialLeaseError::MissingSlot)
    ));
    let debug = format!("{bundle:?}");
    assert!(!debug.contains("bot-secret"));
    assert!(!debug.contains("webhook-secret"));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn bundle_rejects_long_ttl_duplicate_slots_and_non_utf8() {
    assert!(matches!(
        CredentialLeaseBundle::new(1, 1, 100, 401, [("token".into(), b"secret".to_vec())]),
        Err(CredentialLeaseError::InvalidLease)
    ));
    assert!(matches!(
        CredentialLeaseBundle::new(
            1,
            1,
            100,
            110,
            [
                ("token".into(), b"one".to_vec()),
                ("token".into(), b"two".to_vec())
            ]
        ),
        Err(CredentialLeaseError::InvalidLease)
    ));
    assert!(matches!(
        CredentialLeaseBundle::new(1, 1, 100, 110, [("token".into(), vec![0xff])]),
        Err(CredentialLeaseError::InvalidEncoding)
    ));
}
