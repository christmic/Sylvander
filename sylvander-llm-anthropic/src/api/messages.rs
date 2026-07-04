//! Messages API surface — `POST /v1/messages` and `POST /v1/messages/count_tokens`.
//!
//! Implementations land in subsequent commits (C5 for `count_tokens`,
//! C6 for `create`, C8 for `stream`).

use crate::api::client::AnthropicClient;

/// Bound handle to the Messages API. Returned by
/// [`AnthropicClient::messages`] and borrows the client for `'a`.
pub struct MessagesApi<'a> {
    #[allow(dead_code)]
    client: &'a AnthropicClient,
}

impl<'a> MessagesApi<'a> {
    /// Construct a new bound Messages API. Internal — callers obtain
    /// this via [`AnthropicClient::messages`].
    pub(crate) fn new(client: &'a AnthropicClient) -> Self {
        Self { client }
    }
}