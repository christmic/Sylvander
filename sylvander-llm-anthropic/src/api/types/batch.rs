//! Message Batch API types.
//!
//! The Message Batches API (`POST /v1/messages/batches`) lets you send
//! up to a batch of requests at 50% discount. Results are available
//! via polling or by downloading a `.jsonl` file from `results_url`.
//!
//! Wire format is per the Anthropic Messages Batches documentation. All
//! timestamp fields are RFC 3339 strings; callers can parse them with
//! their preferred datetime library (chrono, time, etc.). The SDK keeps
//! them as `String` to avoid taking on a dependency.

use serde::{Deserialize, Serialize};

use super::message::Message;
use crate::api::request::CreateMessageRequest;

/// ISO 8601 / RFC 3339 timestamp string.
pub type Timestamp = String;

/// A Message Batch — a collection of message generation requests
/// processed asynchronously.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBatch {
    /// Unique batch identifier (`msgbatch_xxx`).
    pub id: String,
    /// Always `"message_batch"`.
    #[serde(rename = "type")]
    pub kind: MessageBatchKind,
    /// When the batch was created.
    pub created_at: Timestamp,
    /// When the batch expires (24 hours after creation).
    pub expires_at: Timestamp,
    /// Processing status.
    pub processing_status: ProcessingStatus,
    /// Tallies of requests by status.
    pub request_counts: MessageBatchRequestCounts,
    /// When the batch was archived and results became unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<Timestamp>,
    /// When cancellation was initiated (if applicable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_initiated_at: Option<Timestamp>,
    /// When batch processing ended (if applicable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Timestamp>,
    /// URL to a `.jsonl` file containing batch results (if ended).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results_url: Option<String>,
}

/// Message Batch discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageBatchKind {
    /// Message batch object.
    #[serde(rename = "message_batch")]
    MessageBatch,
}

/// Processing status of a Message Batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStatus {
    /// Batch is still being processed.
    InProgress,
    /// Cancellation has been initiated but processing hasn't fully
    /// stopped yet.
    Canceling,
    /// Processing has ended — all requests are in a final state.
    Ended,
}

/// Tallies of requests in a batch, categorized by status.
///
/// All counts are 0 while the batch is still processing. Once the batch
/// ends, the sum of all values equals the total number of requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MessageBatchRequestCounts {
    /// Number of canceled requests.
    pub canceled: u32,
    /// Number of errored requests.
    pub errored: u32,
    /// Number of expired requests.
    pub expired: u32,
    /// Number of requests still being processed.
    pub processing: u32,
    /// Number of successfully completed requests.
    pub succeeded: u32,
}

/// Parameters for creating a new Message Batch.
///
/// Used as the request body of `POST /v1/messages/batches`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMessageBatchRequest {
    /// List of requests in the batch. Each request gets a
    /// developer-provided `custom_id` for matching results.
    pub requests: Vec<BatchRequest>,
}

/// A single request in a batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequest {
    /// Developer-provided unique ID for this request within the batch.
    /// Used to match results to requests in the output `.jsonl`.
    pub custom_id: String,
    /// The message generation params (same as `CreateMessageRequest`).
    pub params: CreateMessageRequest,
}

/// Result of processing a single request in a batch (one line in the
/// `.jsonl` results file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBatchIndividualResponse {
    /// Developer-provided ID matching this result to the original request.
    pub custom_id: String,
    /// Processing result — succeeded, errored, canceled, or expired.
    pub result: MessageBatchResult,
}

/// Per-request processing result. Untagged union over the four possible
/// outcomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageBatchResult {
    /// Request succeeded — `message` carries the response.
    Succeeded {
        /// The model response.
        message: Message,
    },
    /// Request errored — `error` carries the API error response.
    Errored {
        /// The error details.
        error: BatchError,
    },
    /// Request was canceled.
    Canceled,
    /// Request expired before processing.
    Expired,
}

/// Error response for an errored batch request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchError {
    /// Always `"error"`.
    #[serde(rename = "type")]
    pub kind: BatchErrorKind,
    /// Error message.
    pub message: String,
}

/// Batch error discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchErrorKind {
    /// Error object.
    #[serde(rename = "error")]
    Error,
}

/// Pagination parameters for listing batches.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListBatchesParams {
    /// Cursor for pagination (the `before_id` / `after_id` from a
    /// previous response).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Only return batches created before this ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_id: Option<String>,
    /// Only return batches created after this ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_id: Option<String>,
}

/// Paginated list of message batches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBatchesPage {
    /// The list of batches.
    pub data: Vec<MessageBatch>,
    /// Whether more results are available (`true` if there is a next page).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_more: bool,
    /// First ID in the list (for pagination cursors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_id: Option<String>,
    /// Last ID in the list (for pagination cursors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_id: Option<String>,
}

#[cfg(test)]
#[path = "../../../tests/unit/api_types_batch.rs"]
mod tests;
