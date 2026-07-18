//! Object-safe provider invocation boundary.

use std::future::Future;
use std::pin::Pin;

use futures_util::Stream;

use crate::{ModelInfo, ModelRequest, ModelStreamEvent, ProviderError};

pub type ModelEventStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ProviderError>> + Send + 'static>>;

pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ModelEventStream, ProviderError>> + Send + 'a>>;

pub type ModelCatalogFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Vec<ModelInfo>>, ProviderError>> + Send + 'a>>;

/// One model-provider adapter.
///
/// Implementations normalize streaming and buffered transports, but do not
/// retry. Retry policy belongs to the Agent loop.
pub trait ModelProvider: Send + Sync {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_>;

    /// Enumerate remote models only when the provider has a reliable catalog
    /// contract. `None` keeps the operator-managed Registry authoritative.
    fn model_catalog(&self) -> ModelCatalogFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}
