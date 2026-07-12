//! Isolated lifecycle boundary for model-requested background work.

use async_trait::async_trait;

#[async_trait]
pub trait TaskGate: Send + Sync {
    async fn start(&self, purpose: String, prompt: String) -> Result<String, String>;
}
