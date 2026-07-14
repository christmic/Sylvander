//! Object-safe provider invocation boundary.

use std::future::Future;
use std::pin::Pin;

use futures_util::Stream;

use crate::{ModelRequest, ModelStreamEvent, ProviderError};

pub type ModelEventStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ProviderError>> + Send + 'static>>;

pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ModelEventStream, ProviderError>> + Send + 'a>>;

/// One model-provider adapter.
///
/// Implementations normalize streaming and buffered transports, but do not
/// retry. Retry policy belongs to the Agent loop.
pub trait ModelProvider: Send + Sync {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{ModelRef, ModelResponse, StopReason, TokenUsage};
    use futures_util::{StreamExt, stream};

    struct FakeProvider;

    impl ModelProvider for FakeProvider {
        fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
            Box::pin(async move {
                let response = ModelResponse {
                    id: request.request_id,
                    model: request.model,
                    content: Vec::new(),
                    stop_reason: StopReason::EndTurn,
                    usage: TokenUsage::default(),
                };
                let stream: ModelEventStream =
                    Box::pin(stream::iter([Ok(ModelStreamEvent::Completed(response))]));
                Ok(stream)
            })
        }
    }

    #[tokio::test]
    async fn trait_is_object_safe_and_stream_is_owned() {
        let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider);
        let request = ModelRequest {
            request_id: "request-1".into(),
            model: ModelRef::new("fake", "model"),
            system: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_output_tokens: 1,
            reasoning: None,
            output_schema: None,
        };
        let events = provider
            .complete_stream(request)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(
            events.as_slice(),
            [Ok(ModelStreamEvent::Completed(response))]
                if response.model == ModelRef::new("fake", "model")
        ));
    }
}
