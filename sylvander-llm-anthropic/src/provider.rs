//! Provider-neutral adapter backed by the Anthropic Messages API.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::Stream;
use sylvander_llm_core::{
    ModelEventStream, ModelProvider, ModelRequest, ModelStreamEvent, ProviderError,
    ProviderErrorKind, ProviderErrorPhase, ProviderFuture,
};

use crate::api::client::AnthropicClient;
use crate::api::message_stream::MessageStream;
use crate::api::types::{ContentDelta, RawStreamEvent};
use crate::convert;

#[derive(Clone, Debug)]
pub struct AnthropicProvider {
    id: String,
    client: AnthropicClient,
}

impl AnthropicProvider {
    #[must_use]
    pub fn new(id: impl Into<String>, client: AnthropicClient) -> Self {
        Self {
            id: id.into(),
            client,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl ModelProvider for AnthropicProvider {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
        Box::pin(async move {
            if request.model.provider != self.id {
                return Err(ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    ProviderErrorPhase::Open,
                    "model provider does not match adapter",
                ));
            }
            let request = convert::request(&request)?;
            let stream = self
                .client
                .messages()
                .stream(&request)
                .await
                .map_err(|error| convert::error(error, ProviderErrorPhase::Open))?;
            Ok(Box::pin(NeutralStream {
                inner: stream,
                provider: self.id.clone(),
                terminated: false,
            }) as ModelEventStream)
        })
    }
}

struct NeutralStream {
    inner: MessageStream,
    provider: String,
    terminated: bool,
}

impl Stream for NeutralStream {
    type Item = Result<ModelStreamEvent, ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.terminated {
            return Poll::Ready(None);
        }
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(RawStreamEvent::ContentBlockDelta { delta, .. }))) => {
                    match delta {
                        ContentDelta::TextDelta { text } => {
                            return Poll::Ready(Some(Ok(ModelStreamEvent::TextDelta(text))));
                        }
                        ContentDelta::ThinkingDelta { thinking } => {
                            return Poll::Ready(Some(Ok(ModelStreamEvent::ReasoningDelta(
                                thinking,
                            ))));
                        }
                        _ => {}
                    }
                }
                Poll::Ready(Some(Ok(_))) => {}
                Poll::Ready(Some(Err(error))) => {
                    self.terminated = true;
                    return Poll::Ready(Some(Err(convert::error(
                        error,
                        ProviderErrorPhase::Stream,
                    ))));
                }
                Poll::Ready(None) => {
                    self.terminated = true;
                    let Some(message) = self.inner.final_message() else {
                        return Poll::Ready(Some(Err(ProviderError::new(
                            ProviderErrorKind::Protocol,
                            ProviderErrorPhase::Stream,
                            "model provider stream ended before completion",
                        ))));
                    };
                    return Poll::Ready(Some(Ok(ModelStreamEvent::Completed(convert::response(
                        &self.provider,
                        message,
                    )))));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/provider.rs"]
mod tests;
