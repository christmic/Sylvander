use super::*;
use sylvander_protocol::{
    AuthenticatedPrincipal, AuthenticationMethod, IDENTITY_BINDING_PROTOCOL_VERSION,
    IdentityBindingAction,
};

fn boundary(request: &str) -> BoundaryContext {
    BoundaryContext::authenticated(
        AuthenticatedPrincipal::user("alice", AuthenticationMethod::BearerToken),
        "desktop",
        "websocket",
        request,
    )
}

#[tokio::test]
async fn payload_and_rate_limits_fail_before_dispatch() {
    let guard = BoundaryGuard::new(BoundarySettings {
        max_request_bytes: 64,
        requests_per_minute: 1,
    });
    guard
        .check(&boundary("one"), &UiClientMessage::Ping, "ping")
        .await
        .unwrap();
    let error = guard
        .check(&boundary("two"), &UiClientMessage::Ping, "ping")
        .await
        .unwrap_err();
    assert_eq!(error.code, BoundaryErrorCode::RateLimited);

    let error = guard
        .check(
            &boundary("large"),
            &UiClientMessage::Chat {
                text: "x".repeat(128),
                attachments: Vec::new(),
                session_id: None,
                workspace: None,
            },
            "chat",
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, BoundaryErrorCode::PayloadTooLarge);
}

#[tokio::test]
async fn expired_rate_keys_are_cleaned_periodically() {
    let guard = BoundaryGuard::new(BoundarySettings::default());
    let now = Instant::now();
    {
        let mut state = guard.rate_state.lock().await;
        state.windows.insert(
            "expired".into(),
            RateWindow {
                started: now
                    .checked_sub(RATE_WINDOW + Duration::from_secs(1))
                    .unwrap(),
                requests: 1,
            },
        );
        state.windows.insert(
            "fresh".into(),
            RateWindow {
                started: now,
                requests: 1,
            },
        );
        state.last_cleanup = now.checked_sub(CLEANUP_INTERVAL).unwrap();
    }

    guard
        .check(&boundary("cleanup"), &UiClientMessage::Ping, "ping")
        .await
        .unwrap();
    let state = guard.rate_state.lock().await;
    assert!(!state.windows.contains_key("expired"));
    assert!(state.windows.contains_key("fresh"));
    assert_eq!(state.windows.len(), 2);
}

#[tokio::test]
async fn authentication_failures_share_the_anonymous_rate_window() {
    let guard = BoundaryGuard::new(BoundarySettings {
        max_request_bytes: 1024,
        requests_per_minute: 1,
    });
    let first = BoundaryContext::unauthenticated("desktop", "websocket", "auth-one");
    guard
        .check_authentication_failure(&first, "authenticate_bearer_token")
        .await
        .unwrap();
    let second = BoundaryContext::unauthenticated("desktop", "websocket", "auth-two");
    let error = guard
        .check_authentication_failure(&second, "authenticate_bearer_token")
        .await
        .unwrap_err();
    assert_eq!(error.code, BoundaryErrorCode::RateLimited);
    assert_eq!(error.operation, "authenticate_bearer_token");
}

#[tokio::test]
async fn identity_operations_share_the_authenticated_boundary_rate_limit() {
    let guard = BoundaryGuard::new(BoundarySettings {
        max_request_bytes: 1024,
        requests_per_minute: 1,
    });
    let request = IdentityBindingRequest {
        version: IDENTITY_BINDING_PROTOCOL_VERSION,
        action: IdentityBindingAction::Resolve {},
    };
    guard
        .check_identity(&boundary("identity-one"), &request)
        .await
        .unwrap();
    let error = guard
        .check_identity(&boundary("identity-two"), &request)
        .await
        .unwrap_err();
    assert_eq!(error.code, BoundaryErrorCode::RateLimited);
    assert_eq!(error.operation, "identity_binding");
}
