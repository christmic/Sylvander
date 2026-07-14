//! Shared ingress limits applied before any UI operation is dispatched.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use sylvander_protocol::{BoundaryContext, BoundaryError, BoundaryErrorCode, UiClientMessage};

use crate::config::BoundarySettings;

#[derive(Clone)]
pub(crate) struct BoundaryGuard {
    settings: BoundarySettings,
    windows: Arc<Mutex<HashMap<String, RateWindow>>>,
}

struct RateWindow {
    started: Instant,
    requests: u32,
}

impl BoundaryGuard {
    pub(crate) fn new(settings: BoundarySettings) -> Self {
        Self {
            settings,
            windows: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn check(
        &self,
        boundary: &BoundaryContext,
        message: &UiClientMessage,
        operation: &str,
    ) -> Result<(), BoundaryError> {
        let bytes = serde_json::to_vec(message).map_err(|error| BoundaryError {
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

        let principal = boundary
            .principal
            .as_ref()
            .map_or("__unauthenticated__", |principal| principal.id.0.as_str());
        let key = format!("{}\0{principal}", boundary.channel_instance_id);
        let now = Instant::now();
        let mut windows = self.windows.lock().await;
        let window = windows.entry(key).or_insert(RateWindow {
            started: now,
            requests: 0,
        });
        let elapsed = now.saturating_duration_since(window.started);
        if elapsed >= Duration::from_mins(1) {
            window.started = now;
            window.requests = 0;
        }
        if window.requests >= self.settings.requests_per_minute {
            let retry_after = Duration::from_mins(1).saturating_sub(elapsed);
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
    use sylvander_protocol::{AuthenticatedPrincipal, AuthenticationMethod};

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
}
