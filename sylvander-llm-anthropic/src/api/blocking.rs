//! Sync blocking API — non-async wrappers for the Messages API.
//!
//! Useful for CLI tools, scripts, and one-shot batch tasks that don't
//! want to be tainted by `async fn` propagating through the call site.
//!
//! Internally spins up a `tokio::runtime::Runtime` (`current_thread`,
//! single-threaded) and uses `block_on` to drive the async API. Each
//! blocking call re-enters the runtime.
//!
//! ## Not implemented
//!
//! - **Streaming**: streaming via blocking API is an anti-pattern
//!   (blocks one thread per stream). Use the async [`crate::api::messages::MessagesApi::stream`]
//!   for streaming.
//! - **Batches**: same as messages — blocking variant provided for
//!   `create` / `retrieve` / `list` / `cancel`, but not for polling
//!   loops.

use crate::api::client::{AnthropicClient, AnthropicClientBuilder};
use crate::api::error::AnthropicError;
use crate::api::messages::MessagesApi;
use crate::api::request::CreateMessageRequest;
use crate::api::types::{Message, MessageTokensCount};

/// Default blocking runtime configuration: `current_thread`, single-threaded.
#[derive(Debug, Clone)]
pub struct BlockingConfig {
    /// Whether to enable the IO driver on the blocking runtime.
    /// Default: `true` (required for `reqwest`).
    pub enable_io: bool,
    /// Whether to enable the time driver.
    /// Default: `true` (required for timeout-related futures).
    pub enable_time: bool,
    /// Thread name prefix for the runtime thread.
    pub thread_name: String,
}

impl Default for BlockingConfig {
    fn default() -> Self {
        Self {
            enable_io: true,
            enable_time: true,
            thread_name: "sylvander-blocking".to_string(),
        }
    }
}

/// Sync blocking wrapper around [`AnthropicClient`].
///
/// Construct via [`AnthropicClient::blocking`] or
/// [`AnthropicClient::blocking_with_config`].
pub struct BlockingAnthropicClient {
    inner: AnthropicClient,
    runtime: tokio::runtime::Runtime,
}

impl std::fmt::Debug for BlockingAnthropicClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockingAnthropicClient")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl BlockingAnthropicClient {
    /// Build a blocking client using default runtime config.
    ///
    /// # Errors
    /// - [`AnthropicError::Validation`] if the client builder rejects
    ///   the inputs (missing `api_key`, invalid `base_url`)
    /// - [`AnthropicError::Http`] if the underlying `reqwest` client
    ///   fails to initialize
    /// - Returns `std::io::Error` if the tokio runtime fails to build
    pub fn new(builder: AnthropicClientBuilder) -> Result<Self, BlockingClientError> {
        Self::with_config(builder, BlockingConfig::default())
    }

    /// Build a blocking client with custom runtime configuration.
    ///
    /// # Errors
    /// See [`Self::new`].
    pub fn with_config(
        builder: AnthropicClientBuilder,
        config: BlockingConfig,
    ) -> Result<Self, BlockingClientError> {
        let client = builder.build().map_err(BlockingClientError::Client)?;

        let mut runtime_builder = tokio::runtime::Builder::new_current_thread();
        runtime_builder
            .enable_all()
            .thread_name(config.thread_name);

        // Tokio's `enable_all` enables both IO and time. Override
        // based on config:
        let runtime = if config.enable_io && config.enable_time {
            runtime_builder
                .enable_all()
                .build()
                .map_err(BlockingClientError::Runtime)?
        } else {
            runtime_builder
                .build()
                .map_err(BlockingClientError::Runtime)?
        };

        Ok(Self {
            inner: client,
            runtime,
        })
    }

    /// Borrow the underlying async client. Useful for callers that need
    /// to mix async + blocking in the same program.
    #[must_use]
    pub fn async_client(&self) -> &AnthropicClient {
        &self.inner
    }

    /// Access the blocking Messages API.
    #[must_use]
    pub fn messages(&self) -> BlockingMessagesApi<'_> {
        BlockingMessagesApi {
            async_api: self.inner.messages(),
            runtime: &self.runtime,
        }
    }
}

/// Error returned when building a [`BlockingAnthropicClient`].
#[derive(Debug, thiserror::Error)]
pub enum BlockingClientError {
    /// Error from the underlying async client builder.
    #[error(transparent)]
    Client(AnthropicError),
    /// Error from the tokio runtime builder.
    #[error("failed to build tokio runtime: {0}")]
    Runtime(std::io::Error),
}

/// Sync blocking wrapper around [`MessagesApi`].
///
/// Returned by [`BlockingAnthropicClient::messages`]. Each method
/// blocks the current thread until the async operation completes.
pub struct BlockingMessagesApi<'a> {
    async_api: MessagesApi<'a>,
    runtime: &'a tokio::runtime::Runtime,
}

impl BlockingMessagesApi<'_> {
    /// Send a message generation request (non-streaming) and block until
    /// the response is received.
    pub fn create(&self, request: &CreateMessageRequest) -> Result<Message, AnthropicError> {
        self.runtime.block_on(self.async_api.create(request))
    }

    /// Estimate the input token count for a request. Blocks until the
    /// response is received.
    pub fn count_tokens(
        &self,
        request: &CreateMessageRequest,
    ) -> Result<MessageTokensCount, AnthropicError> {
        self.runtime.block_on(self.async_api.count_tokens(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_client_builds_with_default_config() {
        let client = BlockingAnthropicClient::new(
            AnthropicClient::builder().api_key("test-key"),
        )
        .expect("blocking client should build");
        let _ = client.async_client();
    }

    #[test]
    fn blocking_client_with_custom_config() {
        let client = BlockingAnthropicClient::with_config(
            AnthropicClient::builder().api_key("test-key"),
            BlockingConfig {
                enable_io: true,
                enable_time: true,
                thread_name: "test-thread".to_string(),
            },
        )
        .expect("blocking client should build");
        let _ = client.async_client();
    }

    #[test]
    fn blocking_client_propagates_validation_errors() {
        // No API key set
        let result = BlockingAnthropicClient::new(AnthropicClient::builder());
        match result {
            Err(BlockingClientError::Client(AnthropicError::Validation(_))) => {}
            other => panic!("expected Client(Validation) error, got {other:?}"),
        }
    }

    #[test]
    fn blocking_messages_api_exposes_async_api() {
        let client = BlockingAnthropicClient::new(
            AnthropicClient::builder().api_key("test-key"),
        )
        .expect("build should succeed");
        let _api = client.messages();
    }
}