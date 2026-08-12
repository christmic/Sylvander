//! Typed `DashScope` transport, API, and protocol failures.

use reqwest::Response;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DashScopeError {
    #[error("DashScope transport failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("DashScope JSON was invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("DashScope SSE was invalid: {0}")]
    Sse(String),
    #[error("DashScope API rejected the request with status {status}: {code}")]
    Api {
        status: u16,
        code: String,
        request_id: Option<String>,
    },
    #[error("DashScope protocol violation: {0}")]
    Protocol(String),
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    code: Option<String>,
    request_id: Option<String>,
}

impl DashScopeError {
    pub(crate) async fn from_response(response: Response) -> Self {
        let status = response.status().as_u16();
        let header_id = response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.bytes().await.unwrap_or_default();
        let parsed = serde_json::from_slice::<ErrorBody>(&body).ok();
        Self::Api {
            status,
            code: parsed
                .as_ref()
                .and_then(|value| value.code.clone())
                .unwrap_or_else(|| "Unknown".into()),
            request_id: parsed.and_then(|value| value.request_id).or(header_id),
        }
    }

    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } => Some(*status),
            _ => None,
        }
    }

    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Api { request_id, .. } => request_id.as_deref(),
            _ => None,
        }
    }
}
