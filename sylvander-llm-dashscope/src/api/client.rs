//! Explicitly configured native `DashScope` HTTP client.

use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Response, Url};

use crate::api::{DashScopeError, GenerationRequest};

/// Default wall-clock deadline for one HTTP request and its response stream.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(2);

#[derive(Clone)]
pub struct DashScopeClient {
    endpoint: Url,
    headers: HeaderMap,
    http: reqwest::Client,
}

impl DashScopeClient {
    pub fn new(base_url: Url, api_key: &str) -> Result<Self, DashScopeError> {
        Self::new_with_timeout(base_url, api_key, DEFAULT_TIMEOUT)
    }

    pub fn new_with_timeout(
        base_url: Url,
        api_key: &str,
        timeout: Duration,
    ) -> Result<Self, DashScopeError> {
        if api_key.is_empty() {
            return Err(DashScopeError::Protocol(
                "provider credential is empty".into(),
            ));
        }
        let endpoint = base_url
            .join("api/v1/services/aigc/text-generation/generation")
            .map_err(|_| DashScopeError::Protocol("provider endpoint is invalid".into()))?;
        let mut headers = HeaderMap::new();
        let authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| DashScopeError::Protocol("provider credential is invalid".into()))?;
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert("x-dashscope-sse", HeaderValue::from_static("enable"));
        Ok(Self {
            endpoint,
            headers,
            http: reqwest::Client::builder().timeout(timeout).build()?,
        })
    }

    pub(crate) async fn post(
        &self,
        request: &GenerationRequest,
    ) -> Result<Response, DashScopeError> {
        let response = self
            .http
            .post(self.endpoint.clone())
            .headers(self.headers.clone())
            .json(request)
            .send()
            .await?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(DashScopeError::from_response(response).await)
        }
    }
}

impl std::fmt::Debug for DashScopeClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashScopeClient")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}
