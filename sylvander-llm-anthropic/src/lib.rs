//! # sylvander-llm-anthropic
//!
//! Sylvander v2 Anthropic Protocol SDK — minimal Rust wrapper for the
//! Anthropic Messages API.
//!
//! This crate provides the M1 Protocol SDK layer for the Sylvander v2
//! Agent framework. It implements the wire format for:
//!
//! - `POST /v1/messages` — message generation (sync + streaming SSE)
//! - `POST /v1/messages/count_tokens` — token estimation
//!
//! Scope is deliberately minimal: no files API, no batches, no managed
//! agents platform. See `projects/Sylvander/designs/anthropic-sdk-capabilities.md`
//! in the Oraculo repo for the full capability surface and rationale.
//!
//! ## Quickstart
//!
//! ```no_run
//! use sylvander_llm_anthropic::prelude::*;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let client = AnthropicClient::builder()
//!     .api_key(std::env::var("ANTHROPIC_API_KEY")?)
//!     .build()?;
//!
//! let request = CreateMessageRequest::builder()
//!     .model("claude-sonnet-5-20260601")
//!     .max_tokens(1024)
//!     .messages(vec![MessageParam::user("Hello, Claude")])
//!     .build()
//!     .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
//!
//! let msg = client.messages().create(&request).await?;
//! if let Some(text) = msg.content[0].text() {
//!     println!("{text}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Streaming
//!
//! ```no_run
//! use futures_util::StreamExt;
//! use sylvander_llm_anthropic::prelude::*;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! # let client = AnthropicClient::builder()
//! #     .api_key(std::env::var("ANTHROPIC_API_KEY")?)
//! #     .build()?;
//! let request = CreateMessageRequest::builder()
//!     .model("claude-sonnet-5-20260601")
//!     .max_tokens(1024)
//!     .messages(vec![MessageParam::user("Stream me a story")])
//!     .build()
//!     .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
//!
//! let mut stream = client.messages().stream(&request).await?;
//!
//! while let Some(event) = stream.next().await {
//!     let event = event?;
//!     if let RawStreamEvent::ContentBlockDelta {
//!         delta: ContentDelta::TextDelta { text },
//!         ..
//!     } = &event
//!     {
//!         print!("{text}");
//!     }
//!     if matches!(event, RawStreamEvent::MessageStop) {
//!         break;
//!     }
//! }
//! # Ok(())
//! # }
//! ```

#![doc(html_root_url = "https://docs.rs/sylvander-llm-anthropic/0.1.0")]

pub mod api;

/// Convenient re-exports for the most commonly used types.
pub mod prelude {
    pub use crate::api::batches::BatchesApi;
    pub use crate::api::blocking::{BlockingAnthropicClient, BlockingClientError, BlockingConfig};
    pub use crate::api::client::{AnthropicClient, AnthropicClientBuilder};
    pub use crate::api::error::AnthropicError;
    pub use crate::api::message_stream::MessageStream;
    pub use crate::api::messages::MessagesApi;
    pub use crate::api::model::{ModelCapabilities, ModelInfo, ModelInfoBuilder};
    pub use crate::api::request::{CreateMessageRequest, CreateMessageRequestBuilder};
    pub use crate::api::types::{
        BatchRequest, CacheControl, CacheTtl, CitationCharLocation, CitationContentBlockLocation,
        CitationPageLocation, CitationsSearchResultLocation, CitationsWebSearchResultLocation,
        ContentBlock, ContentDelta, CreateMessageBatchRequest, Effort, ImageBlock, InputSchema,
        JsonOutputFormat, ListBatchesParams, Message, MessageBatch, MessageBatchIndividualResponse,
        MessageBatchKind, MessageBatchRequestCounts, MessageBatchResult, MessageBatchesPage,
        MessageKind, MessageParam, MessageRole, MessageTokensCount, OutputConfig, ProcessingStatus,
        RawStreamEvent, RichToolResultBlock, StopReason, SystemPrompt, SystemTextBlock, TextBlock,
        TextCitation, ThinkingBlock, ThinkingConfig, Timestamp, Tool, ToolChoice, ToolResultBlock,
        ToolUseBlock, Usage, UserContent, UserContentBlock,
    };
}
