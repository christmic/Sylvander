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
//! ```ignore
//! use sylvander_llm_anthropic::prelude::*;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let client = AnthropicClient::builder()
//!     .api_key(std::env::var("ANTHROPIC_API_KEY")?)
//!     .build()?;
//!
//! let msg = client
//!     .messages()
//!     .create(
//!         CreateMessageRequest::builder()
//!             .model(ModelId::ClaudeSonnet5)
//!             .max_tokens(1024)
//!             .messages(vec![MessageParam::user("Hello, Claude")])
//!             .build()?,
//!     )
//!     .await?;
//! println!("{}", msg.content[0].text()); // panics if first block is not text
//! # Ok(())
//! # }
//! ```
//!
//! ## Streaming
//!
//! ```ignore
//! use futures_util::StreamExt;
//! use sylvander_llm_anthropic::prelude::*;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! # let client = AnthropicClient::builder()
//! #     .api_key(std::env::var("ANTHROPIC_API_KEY")?)
//! #     .build()?;
//! let mut stream = client
//!     .messages()
//!     .stream(
//!         CreateMessageRequest::builder()
//!             .model(ModelId::ClaudeSonnet5)
//!             .max_tokens(1024)
//!             .messages(vec![MessageParam::user("Stream me a story")])
//!             .build()?,
//!     )
//!     .await?;
//!
//! while let Some(event) = stream.next().await {
//!     match event? {
//!         StreamEvent::ContentBlockDelta { delta: ContentDelta::TextDelta(t), .. } => {
//!             print!("{}", t.text);
//!         }
//!         StreamEvent::MessageStop => break,
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

#![doc(html_root_url = "https://docs.rs/sylvander-llm-anthropic/0.1.0")]

pub mod api;

/// Convenient re-exports for the most commonly used types.
pub mod prelude {
    pub use crate::api::client::{AnthropicClient, AnthropicClientBuilder};
    pub use crate::api::error::AnthropicError;
    pub use crate::api::model_registry::{ModelCapabilities, ModelId, ModelInfo};
    pub use crate::api::request::{CreateMessageRequest, CreateMessageRequestBuilder};
    pub use crate::api::types::{
        CacheControl, CacheTtl, ContentBlock, Effort, ImageBlock, InputSchema, JsonOutputFormat,
        Message, MessageKind, MessageParam, MessageRole, MessageTokensCount, OutputConfig,
        RichToolResultBlock, StopReason, SystemPrompt, SystemTextBlock, TextBlock, ThinkingBlock,
        ThinkingConfig, Tool, ToolChoice, ToolResultBlock, ToolUseBlock, Usage, UserContent,
        UserContentBlock,
    };
}