//! Anthropic API surface.
//!
//! Modules are organized by responsibility:
//!
//! - [`client`] — `AnthropicClient` and builder
//! - [`error`] — typed error variants
//! - [`messages`] — `POST /v1/messages` (sync + streaming) and `count_tokens`
//! - [`streaming`] — low-level SSE byte parser
//! - [`message_stream`] — `MessageStream` wrapper (`impl Stream` + `final_message`)
//! - [`model_registry`] — hardcoded model metadata
//! - [`request`] — `CreateMessageRequest` and builder
//! - [`response`] — `Message` response type and tokens count
//! - [`types`] — wire-format types (blocks, tools, content, etc.)

pub mod batches;
pub mod blocking;
pub mod client;
pub mod error;
pub mod message_stream;
pub mod messages;
pub mod model;
pub mod request;
pub mod streaming;
pub mod types;
