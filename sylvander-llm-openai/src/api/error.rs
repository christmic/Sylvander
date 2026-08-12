//! Typed `OpenAI` transport, API, and protocol failures.

use reqwest::Response;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OpenAiError {
    #[error("OpenAI transport failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("OpenAI JSON was invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("OpenAI SSE was invalid: {0}")]
    Sse(String),
    #[error("OpenAI API rejected the request with status {status}: {error_type}")]
    Api {
        status: u16,
        error_type: String,
        request_id: Option<String>,
        retry_after_ms: Option<u64>,
    },
    #[error("OpenAI protocol violation: {0}")]
    Protocol(String),
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(rename = "type")]
    error_type: Option<String>,
    code: Option<serde_json::Value>,
}

impl OpenAiError {
    pub(crate) async fn from_response(response: Response) -> Self {
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let retry_after_ms = retry_after_ms(response.headers());
        let body = response.bytes().await.unwrap_or_default();
        let parsed = serde_json::from_slice::<ErrorEnvelope>(&body).ok();
        let error_type = parsed
            .as_ref()
            .and_then(|value| value.error.error_type.clone())
            .or_else(|| {
                parsed.as_ref().and_then(|value| {
                    value
                        .error
                        .code
                        .as_ref()
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
            })
            .unwrap_or_else(|| "unknown_error".into());
        Self::Api {
            status,
            error_type,
            request_id,
            retry_after_ms,
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

    #[must_use]
    pub const fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::Api { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
    }
}

fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    if let Some(value) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
    {
        return Some(value);
    }
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1_000))
}
