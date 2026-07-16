use std::sync::Arc;
use std::time::Duration;

use sylvander_protocol::{
    AuthenticatedPrincipal, AuthenticationMethod, BoundaryContext,
    IDENTITY_BINDING_PROTOCOL_VERSION, IdentityBindingAction, IdentityBindingError,
    IdentityBindingErrorCode, IdentityBindingRequest, IdentityBindingResponse, UserId,
};

use crate::identity_binding_service::{
    IdentityBindingService, IdentityIngress, TrustedIdentityIssuer,
};
use crate::principal_binding::{Clock, PrincipalBindingStore, PrincipalDigestKey};

struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> i64 {
        1_000
    }
}

fn boundary(principal: &str, channel: &str, transport: &str) -> BoundaryContext {
    BoundaryContext::authenticated(
        AuthenticatedPrincipal::user(principal, AuthenticationMethod::PlatformIdentity),
        channel,
        transport,
        "request-1",
    )
}

fn ingress(principal: &str, channel: &str, transport: &str) -> IdentityIngress {
    IdentityIngress::new(transport.into(), channel.into(), principal.into())
}

async fn service() -> IdentityBindingService {
    let store = PrincipalBindingStore::open_in_memory(
        Arc::new(TestClock),
        PrincipalDigestKey::new(b"0123456789abcdef0123456789abcdef").unwrap(),
    )
    .await
    .unwrap();
    store.register_user(UserId::new("alice")).await.unwrap();
    IdentityBindingService::new(
        store,
        [TrustedIdentityIssuer::new(
            "unix".into(),
            "desktop".into(),
            "local-alice".into(),
            UserId::new("alice"),
        )],
        Duration::from_mins(5),
    )
    .unwrap()
}

#[tokio::test]
async fn stable_issuer_and_external_channel_complete_two_sided_linking() {
    let service = service().await;
    let issued = service
        .dispatch(
            &boundary("local-alice", "desktop", "unix"),
            ingress("local-alice", "desktop", "unix"),
            IdentityBindingRequest {
                version: IDENTITY_BINDING_PROTOCOL_VERSION,
                action: IdentityBindingAction::Begin {},
            },
        )
        .await;
    let IdentityBindingResponse::ChallengeIssued {
        challenge_id,
        secret,
        ..
    } = issued
    else {
        panic!("trusted issuer did not receive a challenge: {issued:?}");
    };

    let confirmed = service
        .dispatch(
            &boundary("telegram-42", "bot-primary", "telegram"),
            ingress("telegram-42", "bot-primary", "telegram"),
            IdentityBindingRequest {
                version: IDENTITY_BINDING_PROTOCOL_VERSION,
                action: IdentityBindingAction::Confirm {
                    challenge_id,
                    proof: secret.into_confirmation_proof(),
                },
            },
        )
        .await;
    assert!(matches!(
        confirmed,
        IdentityBindingResponse::Resolved { binding, .. }
            if binding.user_id == UserId::new("alice") && binding.revision == 1
    ));

    let resolved = service
        .dispatch(
            &boundary("telegram-42", "bot-primary", "telegram"),
            ingress("telegram-42", "bot-primary", "telegram"),
            IdentityBindingRequest {
                version: IDENTITY_BINDING_PROTOCOL_VERSION,
                action: IdentityBindingAction::Resolve {},
            },
        )
        .await;
    assert!(matches!(resolved, IdentityBindingResponse::Resolved { .. }));
}

#[tokio::test]
async fn external_or_mismatched_ingress_cannot_issue_for_a_stable_user() {
    let service = service().await;
    let external = service
        .dispatch(
            &boundary("telegram-42", "bot-primary", "telegram"),
            ingress("telegram-42", "bot-primary", "telegram"),
            IdentityBindingRequest {
                version: IDENTITY_BINDING_PROTOCOL_VERSION,
                action: IdentityBindingAction::Begin {},
            },
        )
        .await;
    assert!(matches!(
        external,
        IdentityBindingResponse::Error {
            error: IdentityBindingError {
                code: IdentityBindingErrorCode::Forbidden,
                ..
            },
            ..
        }
    ));

    let mismatched = service
        .dispatch(
            &boundary("local-alice", "desktop", "unix"),
            ingress("local-alice", "other-desktop", "unix"),
            IdentityBindingRequest {
                version: IDENTITY_BINDING_PROTOCOL_VERSION,
                action: IdentityBindingAction::Begin {},
            },
        )
        .await;
    assert!(matches!(
        mismatched,
        IdentityBindingResponse::Error {
            error: IdentityBindingError {
                code: IdentityBindingErrorCode::Forbidden,
                ..
            },
            ..
        }
    ));
}
