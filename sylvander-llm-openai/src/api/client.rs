//! Explicitly configured `OpenAI` HTTP client.

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Response, Url};
use serde::Serialize;

use crate::api::OpenAiError;

#[derive(Clone)]
pub struct OpenAiClient {
    base_url: Url,
    headers: HeaderMap,
    http: reqwest::Client,
}

impl OpenAiClient {
    pub fn new(base_url: Url, api_key: &str) -> Result<Self, OpenAiError> {
        if api_key.is_empty() {
            return Err(OpenAiError::Protocol("provider credential is empty".into()));
        }
        let mut headers = HeaderMap::new();
        let authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| OpenAiError::Protocol("provider credential is invalid".into()))?;
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(Self {
            base_url,
            headers,
            http: reqwest::Client::new(),
        })
    }

    pub(crate) async fn post<T: Serialize + ?Sized>(
        &self,
        path: &str,
        request: &T,
    ) -> Result<Response, OpenAiError> {
        let endpoint = self
            .base_url
            .join(path)
            .map_err(|_| OpenAiError::Protocol("provider endpoint is invalid".into()))?;
        let response = self
            .http
            .post(endpoint)
            .headers(self.headers.clone())
            .json(request)
            .send()
            .await?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(OpenAiError::from_response(response).await)
        }
    }
}

impl std::fmt::Debug for OpenAiClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}
