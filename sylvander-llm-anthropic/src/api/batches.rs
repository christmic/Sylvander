//! Message Batches API surface — `POST /v1/messages/batches` and
//! related endpoints.
//!
//! The Message Batches API lets you submit up to a batch of requests at
//! 50% discount. Results are available via polling [`BatchesApi::retrieve`]
//! or by downloading a `.jsonl` file from the returned `results_url`.

use crate::api::client::AnthropicClient;
use crate::api::error::AnthropicError;
use crate::api::types::{
    CreateMessageBatchRequest, ListBatchesParams, MessageBatch, MessageBatchesPage,
};

/// Bound handle to the Message Batches API. Returned by
/// [`crate::api::messages::MessagesApi::batches`].
pub struct BatchesApi<'a> {
    client: &'a AnthropicClient,
}

impl<'a> BatchesApi<'a> {
    /// Construct a new bound Batches API. Internal — callers obtain
    /// this via [`crate::api::messages::MessagesApi::batches`].
    pub(crate) fn new(client: &'a AnthropicClient) -> Self {
        Self { client }
    }

    /// Create a new Message Batch.
    ///
    /// `POST /v1/messages/batches` — returns the new batch metadata
    /// (initially in `processing_status: "in_progress"`).
    pub async fn create(
        &self,
        request: &CreateMessageBatchRequest,
    ) -> Result<MessageBatch, AnthropicError> {
        let url = self
            .client
            .base_url()
            .join("v1/messages/batches")
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
            return Err(super::messages::parse_api_error(status.as_u16(), &bytes));
        }

        let batch: MessageBatch = serde_json::from_slice(&bytes)?;
        Ok(batch)
    }

    /// Retrieve a Message Batch by ID.
    ///
    /// `GET /v1/messages/batches/{id}` — poll this to check status.
    /// When `processing_status` becomes `"ended"`, `results_url` is set
    /// and you can download the `.jsonl` file.
    pub async fn retrieve(&self, batch_id: &str) -> Result<MessageBatch, AnthropicError> {
        let url = self
            .client
            .base_url()
            .join(&format!("v1/messages/batches/{batch_id}"))
            .map_err(|e| AnthropicError::Validation(format!("invalid URL: {e}")))?;

        let response = self
            .client
            .http()
            .get(url)
            .headers(self.client.build_headers())
            .send()
            .await?;

        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            return Err(super::messages::parse_api_error(status.as_u16(), &bytes));
        }

        let batch: MessageBatch = serde_json::from_slice(&bytes)?;
        Ok(batch)
    }

    /// List Message Batches with optional pagination.
    ///
    /// `GET /v1/messages/batches?limit=...&before_id=...&after_id=...`
    pub async fn list(
        &self,
        params: Option<&ListBatchesParams>,
    ) -> Result<MessageBatchesPage, AnthropicError> {
        let mut url = self
            .client
            .base_url()
            .join("v1/messages/batches")
            .map_err(|e| AnthropicError::Validation(format!("invalid URL: {e}")))?;

        if let Some(p) = params {
            let mut query_pairs = url.query_pairs_mut();
            if let Some(limit) = p.limit {
                query_pairs.append_pair("limit", &limit.to_string());
            }
            if let Some(before) = &p.before_id {
                query_pairs.append_pair("before_id", before);
            }
            if let Some(after) = &p.after_id {
                query_pairs.append_pair("after_id", after);
            }
            drop(query_pairs);
        }

        let response = self
            .client
            .http()
            .get(url)
            .headers(self.client.build_headers())
            .send()
            .await?;

        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            return Err(super::messages::parse_api_error(status.as_u16(), &bytes));
        }

        let page: MessageBatchesPage = serde_json::from_slice(&bytes)?;
        Ok(page)
    }

    /// Cancel an in-progress Message Batch.
    ///
    /// `POST /v1/messages/batches/{id}/cancel` — returns the batch
    /// with `processing_status: "canceling"` (transitions to `"ended"`
    /// asynchronously).
    pub async fn cancel(&self, batch_id: &str) -> Result<MessageBatch, AnthropicError> {
        let url = self
            .client
            .base_url()
            .join(&format!("v1/messages/batches/{batch_id}/cancel"))
            .map_err(|e| AnthropicError::Validation(format!("invalid URL: {e}")))?;

        let response = self
            .client
            .http()
            .post(url)
            .headers(self.client.build_headers())
            .send()
            .await?;

        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            return Err(super::messages::parse_api_error(status.as_u16(), &bytes));
        }

        let batch: MessageBatch = serde_json::from_slice(&bytes)?;
        Ok(batch)
    }
}