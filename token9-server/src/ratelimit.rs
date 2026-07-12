use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use token9_contracts::{RateLimitDto, RateLimitsResponse};

use crate::AppState;
use crate::config::Dialect;
use crate::error::AppError;

/// Parsed rate-limit snapshot from upstream response headers. Provider and
/// timestamp are attached by the store on write.
#[derive(Debug, Clone, Default)]
pub struct RateLimitSnapshot {
    pub requests_limit: Option<i64>,
    pub requests_remaining: Option<i64>,
    pub requests_reset: Option<String>,
    pub tokens_limit: Option<i64>,
    pub tokens_remaining: Option<i64>,
    pub tokens_reset: Option<String>,
    /// JSON object of every header whose name contains "ratelimit" (forward-compat).
    pub raw: String,
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn header_i64(headers: &HeaderMap, name: &str) -> Option<i64> {
    header(headers, name).and_then(|s| s.trim().parse().ok())
}

/// Extract a rate-limit snapshot from response headers. Returns `None` when the
/// response carries no rate-limit headers at all (nothing to record).
pub fn parse(dialect: Dialect, headers: &HeaderMap) -> Option<RateLimitSnapshot> {
    // Collect every ratelimit-ish header for the raw blob first.
    let mut raw_map = serde_json::Map::new();
    for (name, value) in headers.iter() {
        let n = name.as_str();
        if n.contains("ratelimit") {
            if let Ok(v) = value.to_str() {
                raw_map.insert(n.to_string(), serde_json::Value::String(v.to_string()));
            }
        }
    }
    if raw_map.is_empty() {
        return None;
    }

    let (req_l, req_r, req_reset, tok_l, tok_r, tok_reset) = if dialect.is_openai() {
        (
            "x-ratelimit-limit-requests",
            "x-ratelimit-remaining-requests",
            "x-ratelimit-reset-requests",
            "x-ratelimit-limit-tokens",
            "x-ratelimit-remaining-tokens",
            "x-ratelimit-reset-tokens",
        )
    } else {
        (
            "anthropic-ratelimit-requests-limit",
            "anthropic-ratelimit-requests-remaining",
            "anthropic-ratelimit-requests-reset",
            "anthropic-ratelimit-tokens-limit",
            "anthropic-ratelimit-tokens-remaining",
            "anthropic-ratelimit-tokens-reset",
        )
    };

    Some(RateLimitSnapshot {
        requests_limit: header_i64(headers, req_l),
        requests_remaining: header_i64(headers, req_r),
        requests_reset: header(headers, req_reset).map(String::from),
        tokens_limit: header_i64(headers, tok_l),
        tokens_remaining: header_i64(headers, tok_r),
        tokens_reset: header(headers, tok_reset).map(String::from),
        raw: serde_json::Value::Object(raw_map).to_string(),
    })
}

/// GET /ratelimits — latest captured vendor rate-limit snapshot per provider.
pub async fn list(State(state): State<AppState>) -> Result<Json<RateLimitsResponse>, AppError> {
    let rows = state
        .store
        .list_rate_limits()
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rate_limits = rows
        .into_iter()
        .map(|r| RateLimitDto {
            provider: r.provider,
            updated_at: r.updated_at,
            requests_limit: r.requests_limit.map(|v| v as i32),
            requests_remaining: r.requests_remaining.map(|v| v as i32),
            requests_reset: r.requests_reset,
            tokens_limit: r.tokens_limit.map(|v| v as i32),
            tokens_remaining: r.tokens_remaining.map(|v| v as i32),
            tokens_reset: r.tokens_reset,
        })
        .collect();
    Ok(Json(RateLimitsResponse { rate_limits }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hm(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    #[test]
    fn parses_anthropic() {
        let h = hm(&[
            ("anthropic-ratelimit-requests-limit", "1000"),
            ("anthropic-ratelimit-requests-remaining", "999"),
            ("anthropic-ratelimit-requests-reset", "2026-07-08T10:00:00Z"),
            ("anthropic-ratelimit-tokens-limit", "80000"),
            ("anthropic-ratelimit-tokens-remaining", "79000"),
            ("content-type", "application/json"),
        ]);
        let s = parse(Dialect::Anthropic, &h).unwrap();
        assert_eq!(s.requests_limit, Some(1000));
        assert_eq!(s.requests_remaining, Some(999));
        assert_eq!(s.requests_reset.as_deref(), Some("2026-07-08T10:00:00Z"));
        assert_eq!(s.tokens_limit, Some(80000));
        assert_eq!(s.tokens_remaining, Some(79000));
        assert!(s.raw.contains("anthropic-ratelimit-requests-limit"));
    }

    #[test]
    fn parses_openai() {
        let h = hm(&[
            ("x-ratelimit-limit-requests", "500"),
            ("x-ratelimit-remaining-requests", "499"),
            ("x-ratelimit-reset-requests", "6m0s"),
            ("x-ratelimit-remaining-tokens", "120000"),
        ]);
        let s = parse(Dialect::OpenaiChat, &h).unwrap();
        assert_eq!(s.requests_limit, Some(500));
        assert_eq!(s.requests_remaining, Some(499));
        assert_eq!(s.requests_reset.as_deref(), Some("6m0s"));
        assert_eq!(s.tokens_remaining, Some(120000));
    }

    #[test]
    fn none_when_absent() {
        let h = hm(&[("content-type", "application/json")]);
        assert!(parse(Dialect::Anthropic, &h).is_none());
    }
}
