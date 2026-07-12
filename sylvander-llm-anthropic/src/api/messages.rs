//! Messages API surface — `POST /v1/messages` and `POST /v1/messages/count_tokens`.

use reqwest::Url;
use serde::Deserialize;
use serde_json::json;

use crate::api::batches::BatchesApi;
use crate::api::client::AnthropicClient;
use crate::api::error::AnthropicError;
use crate::api::message_stream::MessageStream;
use crate::api::request::CreateMessageRequest;
use crate::api::types::{Message, MessageTokensCount};

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

    /// Access the Message Batches API (create / retrieve / list /
    /// cancel).
    #[must_use]
    pub fn batches(&self) -> BatchesApi<'_> {
        BatchesApi::new(self.client)
    }

    /// Send a message generation request (non-streaming) and return the
    /// assembled [`Message`].
    ///
    /// # Errors
    /// - [`AnthropicError::Validation`] if the request fails client-side
    ///   validation
    /// - [`AnthropicError::Api`] for 4xx/5xx responses
    /// - [`AnthropicError::Http`] for transport failures
    pub async fn create(&self, request: &CreateMessageRequest) -> Result<Message, AnthropicError> {
        request.validate()?;

        let url = self
            .client
            .base_url()
            .join("v1/messages")
            .map_err(|e| AnthropicError::Validation(format!("invalid URL: {e}")))?;

        let response = self
            .client
            .http()
            .post(url)
            .headers(self.client.build_request_headers(request))
            .json(request)
            .send()
            .await?;

        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            return Err(parse_api_error(status.as_u16(), &bytes));
        }

        let message: Message = serde_json::from_slice(&bytes)?;
        Ok(message)
    }

    /// Send a message generation request and return a streaming response
    /// as a [`MessageStream`]. The stream yields raw [`crate::api::types::RawStreamEvent`]s.
    /// Call [`MessageStream::final_message`] after the stream completes
    /// to get the assembled [`Message`].
    ///
    /// # Errors
    /// - [`AnthropicError::Validation`] if the request fails client-side
    ///   validation
    /// - [`AnthropicError::Api`] for 4xx/5xx responses (caught before
    ///   the stream is constructed)
    /// - [`AnthropicError::Http`] for transport failures
    pub async fn stream(
        &self,
        request: &CreateMessageRequest,
    ) -> Result<MessageStream, AnthropicError> {
        request.validate()?;

        let url = self
            .client
            .base_url()
            .join("v1/messages")
            .map_err(|e| AnthropicError::Validation(format!("invalid URL: {e}")))?;

        // Serialize the request with `stream: true` appended.
        let mut body = serde_json::to_value(request)?;
        body["stream"] = json!(true);

        let response = self
            .client
            .http()
            .post(url)
            .headers(self.client.build_request_headers(request))
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let bytes = response.bytes().await?;
            return Err(parse_api_error(status.as_u16(), &bytes));
        }

        // Verify the response is an event stream. If it's a regular
        // JSON response (e.g., from a test mock that doesn't bother
        // with SSE wrapping), synthesize a single-event stream from
        // the assembled Message. This lets the same loop work with
        // both real streaming responses and convenient test setups.
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if content_type.starts_with("text/event-stream") {
            Ok(MessageStream::new(response))
        } else {
            let bytes = response.bytes().await?;
            let message: Message = serde_json::from_slice(&bytes).map_err(|e| {
                AnthropicError::Validation(format!(
                    "non-SSE response failed to parse as Message: {e}"
                ))
            })?;
            Ok(MessageStream::from_message(message))
        }
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
            .headers(self.client.build_request_headers(request))
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
        base.join(path)
            .map_err(|e| AnthropicError::Validation(format!("invalid URL: {e}")))
    }
}

/// Parse an Anthropic API error response body into [`AnthropicError::Api`].
pub(crate) fn parse_api_error(status: u16, bytes: &[u8]) -> AnthropicError {
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
