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
#[path = "../tests/unit/boundary.rs"]
mod tests;
