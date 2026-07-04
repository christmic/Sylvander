//! Messages API surface — `POST /v1/messages` and `POST /v1/messages/count_tokens`.
//!
//! Streaming (`stream()`) lands in C8.

use reqwest::Url;
use serde::Deserialize;

use crate::api::client::AnthropicClient;
use crate::api::error::AnthropicError;
use crate::api::request::CreateMessageRequest;
use crate::api::types::MessageTokensCount;

/// Bound handle to the Messages API. Returned by
/// [`AnthropicClient::messages`] and borrows the client for `'a`.
pub struct MessagesApi<'a> {
    client: &'a AnthropicClient,
}

impl<'a> MessagesApi<'a> {
    /// Construct a new bound Messages API. Internal — callers obtain
    /// this via [`AnthropicClient::messages`].
    pub(crate) fn new(client: &'a AnthropicClient) -> Self {
        Self { client }
    }

    /// Estimate the number of input tokens for a request without sending
    /// it. The request is validated client-side first; a successful
    /// response carries a [`MessageTokensCount`] with `input_tokens`.
    ///
    /// # Errors
    /// - [`AnthropicError::Validation`] if the request fails client-side
    ///   validation (empty messages, out-of-range sampling, etc.)
    /// - [`AnthropicError::Api`] for 4xx/5xx responses
    /// - [`AnthropicError::Http`] for transport failures
    pub async fn count_tokens(
        &self,
        request: &CreateMessageRequest,
    ) -> Result<MessageTokensCount, AnthropicError> {
        request.validate()?;

        let url = self
            .client
            .base_url()
            .join("v1/messages/count_tokens")
            .map_err(|e| AnthropicError::Validation(format!("invalid URL: {e}")))?;

        let response = self
            .client
            .http()
            .post(url)
            .headers(self.client.build_headers())
            .json(request)
            .send()
            .await?;

        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            return Err(parse_api_error(status.as_u16(), &bytes));
        }

        let count: MessageTokensCount = serde_json::from_slice(&bytes)?;
        Ok(count)
    }

    /// Internal helper exposed for unit testing — given a base URL,
    /// produce the full URL for an endpoint.
    #[allow(dead_code)]
    pub(crate) fn endpoint_url(base: &Url, path: &str) -> Result<Url, AnthropicError> {
        base.join(path).map_err(|e| AnthropicError::Validation(format!("invalid URL: {e}")))
    }
}

/// Parse an Anthropic API error response body into [`AnthropicError::Api`].
fn parse_api_error(status: u16, bytes: &[u8]) -> AnthropicError {
    #[derive(Deserialize)]
    struct ApiErrorBody {
        #[serde(default)]
        r#type: Option<String>,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        request_id: Option<String>,
    }

    match serde_json::from_slice::<ApiErrorBody>(bytes) {
        Ok(body) => AnthropicError::Api {
            status,
            error_type: body.r#type.unwrap_or_else(|| "unknown".into()),
            error_message: body.message.unwrap_or_else(|| "(no message)".into()),
            request_id: body.request_id,
        },
        Err(_) => AnthropicError::Api {
            status,
            error_type: "unparseable".into(),
            error_message: String::from_utf8_lossy(bytes).into_owned(),
            request_id: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_api_error_with_full_body() {
        let body = br#"{"type":"invalid_request_error","message":"model is required","request_id":"req_abc"}"#;
        let err = parse_api_error(400, body);
        match err {
            AnthropicError::Api {
                status,
                error_type,
                error_message,
                request_id,
            } => {
                assert_eq!(status, 400);
                assert_eq!(error_type, "invalid_request_error");
                assert_eq!(error_message, "model is required");
                assert_eq!(request_id.as_deref(), Some("req_abc"));
            }
            other => panic!("expected Api variant, got {other:?}"),
        }
    }

    #[test]
    fn parse_api_error_with_minimal_body() {
        let body = br#"{"type":"rate_limit_error","message":"slow down"}"#;
        let err = parse_api_error(429, body);
        match err {
            AnthropicError::Api {
                status,
                error_type,
                error_message,
                request_id,
            } => {
                assert_eq!(status, 429);
                assert_eq!(error_type, "rate_limit_error");
                assert_eq!(error_message, "slow down");
                assert!(request_id.is_none());
            }
            other => panic!("expected Api variant, got {other:?}"),
        }
    }

    #[test]
    fn parse_api_error_with_garbage_body() {
        let body = b"not json at all";
        let err = parse_api_error(500, body);
        match err {
            AnthropicError::Api {
                status,
                error_type,
                error_message,
                ..
            } => {
                assert_eq!(status, 500);
                assert_eq!(error_type, "unparseable");
                assert!(error_message.contains("not json"));
            }
            other => panic!("expected Api variant, got {other:?}"),
        }
    }

    #[test]
    fn endpoint_url_appends_path() {
        let base = Url::parse("https://api.anthropic.com/").unwrap();
        let url = MessagesApi::endpoint_url(&base, "v1/messages/count_tokens").unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.anthropic.com/v1/messages/count_tokens"
        );
    }
}