//! Isolated lifecycle boundary for model-requested background work.

use async_trait::async_trait;

/// Stable model intent for work that may outlive the initiating turn.
///
/// `invocation_id` is the durable idempotency key. Runtime implementations
/// must use it when materializing the task instead of allocating a fresh ID
/// after every retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundTaskRequest {
    pub invocation_id: String,
    pub purpose: String,
    pub prompt: String,
}

#[async_trait]
pub trait TaskGate: Send + Sync {
    async fn start(&self, request: BackgroundTaskRequest) -> Result<String, String>;
}
