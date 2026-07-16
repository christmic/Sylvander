//! Shared ingress limits applied before any UI operation is dispatched.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use sylvander_protocol::{
    BoundaryContext, BoundaryError, BoundaryErrorCode, IdentityBindingRequest, UiClientMessage,
};

use crate::config::BoundarySettings;

#[derive(Clone)]
pub(crate) struct BoundaryGuard {
    settings: BoundarySettings,
    rate_state: Arc<Mutex<RateState>>,
}

struct RateWindow {
    started: Instant,
    requests: u32,
}

struct RateState {
    windows: HashMap<String, RateWindow>,
    last_cleanup: Instant,
}

const RATE_WINDOW: Duration = Duration::from_mins(1);
const CLEANUP_INTERVAL: Duration = Duration::from_mins(1);

impl BoundaryGuard {
    pub(crate) fn new(settings: BoundarySettings) -> Self {
        Self {
            settings,
            rate_state: Arc::new(Mutex::new(RateState {
                windows: HashMap::new(),
                last_cleanup: Instant::now(),
            })),
        }
    }

    pub(crate) async fn check(
        &self,
        boundary: &BoundaryContext,
        message: &UiClientMessage,
        operation: &str,
    ) -> Result<(), BoundaryError> {
        self.check_serializable(boundary, message, operation).await
    }

    pub(crate) async fn check_identity(
        &self,
        boundary: &BoundaryContext,
        request: &IdentityBindingRequest,
    ) -> Result<(), BoundaryError> {
        self.check_serializable(boundary, request, "identity_binding")
            .await
    }

    async fn check_serializable(
        &self,
        boundary: &BoundaryContext,
        request: &impl serde::Serialize,
        operation: &str,
    ) -> Result<(), BoundaryError> {
        let bytes = serde_json::to_vec(request).map_err(|error| BoundaryError {
            code: BoundaryErrorCode::InvalidScope,
            operation: operation.into(),
            request_id: boundary.request_id.clone(),
            message: format!("request serialization failed: {error}"),
            retry_after_ms: None,
        })?;
        if bytes.len() > self.settings.max_request_bytes {
            return Err(BoundaryError {
                code: BoundaryErrorCode::PayloadTooLarge,
                operation: operation.into(),
                request_id: boundary.request_id.clone(),
                message: format!(
                    "request exceeds {} byte limit",
                    self.settings.max_request_bytes
                ),
                retry_after_ms: None,
            });
        }

        self.check_rate(boundary, operation).await
    }

    pub(crate) async fn check_authentication_failure(
        &self,
        boundary: &BoundaryContext,
        operation: &str,
    ) -> Result<(), BoundaryError> {
        self.check_rate(boundary, operation).await
    }

    async fn check_rate(
        &self,
        boundary: &BoundaryContext,
        operation: &str,
    ) -> Result<(), BoundaryError> {
        let principal = boundary
            .principal
            .as_ref()
            .map_or("__unauthenticated__", |principal| principal.id.0.as_str());
        let key = format!("{}\0{principal}", boundary.channel_instance_id);
        let now = Instant::now();
        let mut rate_state = self.rate_state.lock().await;
        if now.saturating_duration_since(rate_state.last_cleanup) >= CLEANUP_INTERVAL {
            rate_state
                .windows
                .retain(|_, window| now.saturating_duration_since(window.started) < RATE_WINDOW);
            rate_state.last_cleanup = now;
        }
        let window = rate_state.windows.entry(key).or_insert(RateWindow {
            started: now,
            requests: 0,
        });
        let elapsed = now.saturating_duration_since(window.started);
        if elapsed >= RATE_WINDOW {
            window.started = now;
            window.requests = 0;
        }
        if window.requests >= self.settings.requests_per_minute {
            let retry_after = RATE_WINDOW.saturating_sub(elapsed);
            return Err(BoundaryError {
                code: BoundaryErrorCode::RateLimited,
                operation: operation.into(),
                request_id: boundary.request_id.clone(),
                message: "request rate limit exceeded".into(),
                retry_after_ms: Some(u64::try_from(retry_after.as_millis()).unwrap_or(u64::MAX)),
            });
        }
        window.requests += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
}
