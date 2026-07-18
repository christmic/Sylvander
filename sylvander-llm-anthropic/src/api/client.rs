//! Anthropic API client.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};

use super::error::AnthropicError;
use super::messages::MessagesApi;

/// Default Anthropic API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Default Anthropic API version header value.
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(2);

/// Anthropic API client. Holds an HTTP client and authentication config.
///
/// Clone via `Arc` — sharing a client across tasks is cheap.
#[derive(Clone)]
pub struct AnthropicClient {
    inner: Arc<ClientInner>,
}

impl std::fmt::Debug for AnthropicClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicClient")
            .field("base_url", &self.inner.base_url)
            .field("anthropic_version", &self.inner.anthropic_version)
            .field("beta_headers", &self.inner.beta_headers)
            .finish_non_exhaustive()
    }
}

struct ClientInner {
    http: reqwest::Client,
    base_url: Url,
    api_key: String,
    anthropic_version: String,
    beta_headers: Vec<String>,
}

impl AnthropicClient {
    /// Start building a new client.
    #[must_use]
    pub fn builder() -> AnthropicClientBuilder {
        AnthropicClientBuilder::default()
    }

    /// Access the Messages API (`create` / `stream` / `count_tokens`).
    #[must_use]
    pub fn messages(&self) -> MessagesApi<'_> {
        MessagesApi::new(self)
    }

    /// Build a blocking (sync) wrapper around this client using the
    /// default runtime configuration.
    ///
    /// # Errors
    /// Returns [`super::blocking::BlockingClientError::Client`] if the
    /// client builder rejects the inputs (e.g., missing `api_key`), or
    /// [`super::blocking::BlockingClientError::Runtime`] if the tokio
    /// runtime fails to build.
    pub fn blocking(
        self,
    ) -> Result<super::blocking::BlockingAnthropicClient, super::blocking::BlockingClientError>
    {
        self.blocking_with_config(super::blocking::BlockingConfig::default())
    }

    /// Build a blocking wrapper with a custom runtime configuration.
    ///
    /// # Errors
    /// See [`Self::blocking`].
    pub fn blocking_with_config(
        self,
        config: super::blocking::BlockingConfig,
    ) -> Result<super::blocking::BlockingAnthropicClient, super::blocking::BlockingClientError>
    {
        super::blocking::BlockingAnthropicClient::with_config(self.into_builder(), config)
    }

    /// Consume `self` and produce a builder that reproduces this
    /// client's configuration. Used by `blocking()` to rebuild a
    /// builder from an existing client.
    ///
    /// Note: timeout is not preserved (`reqwest::Client` doesn't expose
    /// it back); the default timeout is used for the rebuilt builder.
    fn into_builder(self) -> AnthropicClientBuilder {
        AnthropicClientBuilder {
            api_key: Some(self.inner.api_key.clone()),
            base_url: self.inner.base_url.to_string(),
            anthropic_version: self.inner.anthropic_version.clone(),
            beta_headers: self.inner.beta_headers.clone(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Base URL for API requests.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.inner.base_url
    }

    /// Anthropic API version header value.
    #[must_use]
    pub fn anthropic_version(&self) -> &str {
        &self.inner.anthropic_version
    }

    /// Beta headers that the client auto-attaches to relevant requests.
    #[must_use]
    pub fn beta_headers(&self) -> &[String] {
        &self.inner.beta_headers
    }

    /// Build the standard headers for a request. Used internally by
    /// [`MessagesApi`] and exposed for testing.
    ///
    /// Does **not** include per-request beta headers — see
    /// [`Self::build_request_headers`] for that.
    #[must_use]
    pub fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", self.inner.api_key)) {
            headers.insert(AUTHORIZATION, v);
        }
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "anthropic-version",
            HeaderValue::from_str(&self.inner.anthropic_version)
                .unwrap_or(HeaderValue::from_static(DEFAULT_ANTHROPIC_VERSION)),
        );
        if !self.inner.beta_headers.is_empty() {
            let combined = self.inner.beta_headers.join(", ");
            if let Ok(v) = HeaderValue::from_str(&combined) {
                headers.insert("anthropic-beta", v);
            }
        }
        headers
    }

    /// Build headers for a specific request, including per-request beta
    /// headers derived from the request fields:
    ///
    /// - `extended-thinking-2025-01-01` when `thinking` is set
    /// - `structured-outputs-2025-06-01` when `output_config` is set
    ///
    /// Always includes the client-level `beta_header(...)` extras and
    /// the base headers.
    #[must_use]
    pub fn build_request_headers(
        &self,
        request: &super::request::CreateMessageRequest,
    ) -> HeaderMap {
        let mut headers = self.build_headers();
        let mut extras: Vec<&str> = self.inner.beta_headers.iter().map(String::as_str).collect();
        if request.thinking.is_some() {
            extras.push("extended-thinking-2025-01-01");
        }
        if request.output_config.is_some() {
            extras.push("structured-outputs-2025-06-01");
        }
        if !extras.is_empty() {
            let combined = extras.join(", ");
            if let Ok(v) = HeaderValue::from_str(&combined) {
                headers.insert("anthropic-beta", v);
            }
        }
        headers
    }

    /// Borrow the inner reqwest client. Used internally by
    /// [`MessagesApi`].
    #[allow(dead_code)]
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }
}

/// Builder for [`AnthropicClient`].
#[derive(Debug, Clone)]
pub struct AnthropicClientBuilder {
    api_key: Option<String>,
    base_url: String,
    anthropic_version: String,
    beta_headers: Vec<String>,
    timeout: Duration,
}

impl Default for AnthropicClientBuilder {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: DEFAULT_BASE_URL.to_string(),
            anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_string(),
            beta_headers: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl AnthropicClientBuilder {
    /// Set the API key. Required.
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Override the API base URL. Defaults to
    /// `https://api.anthropic.com`.
    ///
    /// A trailing slash is automatically appended if missing — without
    /// it, `Url::join("v1/messages")` would treat the last path
    /// segment as a file and replace it. Pass either form; both work.
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        let url = url.into();
        self.base_url = if url.ends_with('/') {
            url
        } else {
            format!("{url}/")
        };
        self
    }

    /// Override the `anthropic-version` header value. Defaults to
    /// `2023-06-01`.
    #[must_use]
    pub fn anthropic_version(mut self, version: impl Into<String>) -> Self {
        self.anthropic_version = version.into();
        self
    }

    /// Add an extra `anthropic-beta` header value. Multiple values are
    /// comma-separated in the final header. The client also auto-attaches
    /// beta headers when request fields require them (extended thinking,
    /// structured output, prompt caching).
    #[must_use]
    pub fn beta_header(mut self, header: impl Into<String>) -> Self {
        self.beta_headers.push(header.into());
        self
    }

    /// Set the request timeout. Defaults to 120 seconds.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Build the client.
    ///
    /// # Errors
    /// - [`AnthropicError::Validation`] if `api_key` is missing
    /// - [`AnthropicError::Http`] if the base URL is invalid or the
    ///   reqwest client fails to initialize
    pub fn build(self) -> Result<AnthropicClient, AnthropicError> {
        let api_key = self
            .api_key
            .ok_or_else(|| AnthropicError::Validation("api_key is required".into()))?;

        let base_url = Url::parse(&self.base_url)
            .map_err(|e| AnthropicError::Validation(format!("invalid base_url: {e}")))?;

        let http = reqwest::Client::builder().timeout(self.timeout).build()?;

        Ok(AnthropicClient {
            inner: Arc::new(ClientInner {
                http,
                base_url,
                api_key,
                anthropic_version: self.anthropic_version,
                beta_headers: self.beta_headers,
            }),
        })
    }
}

#[cfg(test)]
#[path = "../../tests/unit/api_client.rs"]
mod tests;
