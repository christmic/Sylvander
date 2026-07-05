//! Wire-format types for the Anthropic Messages API.
//!
//! Each module owns one slice of the protocol surface:
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`block`] | `ContentBlock` (assistant-side) + `UserContentBlock` (user-side) |
//! | [`cache`] | `CacheControl` + `CacheTtl` for prompt caching |
//! | [`image`] | `ImageBlock` (base64 inline) |
//! | [`message`] | `MessageParam` (input) + `Message` (output) + `MessageTokensCount` |
//! | [`output_config`] | Structured output schema + effort |
//! | [`stop_reason`] | Why the model stopped generating |
//! | [`system_prompt`] | `SystemPrompt` (string or structured blocks) |
//! | [`thinking`] | Extended thinking config |
//! | [`tool`] | Custom function tools + tool choice |
//! | [`tool_result`] | `ToolResultBlock` (user re-feed of tool output) |
//! | [`usage`] | Token usage accounting |

pub mod batch;
pub mod block;
pub mod cache;
pub mod event;
pub mod image;
pub mod message;
pub mod output_config;
pub mod stop_reason;
pub mod system_prompt;
pub mod thinking;
pub mod tool;
pub mod tool_result;
pub mod usage;

pub use batch::{
    BatchError, BatchErrorKind, BatchRequest, CreateMessageBatchRequest, ListBatchesParams,
    MessageBatch, MessageBatchIndividualResponse, MessageBatchKind, MessageBatchRequestCounts,
    MessageBatchResult, MessageBatchesPage, ProcessingStatus, Timestamp,
};
pub use block::{
    ContentBlock, TextBlock, TextBlockKind, ThinkingBlock, ThinkingBlockKind, ToolUseBlock,
    UserContent, UserContentBlock,
};
pub use cache::{CacheControl, CacheControlKind, CacheTtl};
pub use event::{ContentDelta, MessageDelta, MessageDeltaUsage, RawStreamEvent};
pub use image::{Base64ImageSource, ImageBlock, ImageMediaType, ImageSource};
pub use message::{Message, MessageKind, MessageParam, MessageRole, MessageTokensCount};
pub use output_config::{Effort, JsonOutputFormat, JsonOutputFormatKind, OutputConfig};
pub use stop_reason::StopReason;
pub use system_prompt::{SystemBlock, SystemPrompt, SystemTextBlock};
pub use thinking::ThinkingConfig;
pub use tool::{InputSchema, Tool, ToolChoice};
pub use tool_result::{RichToolResultBlock, ToolResultBlock, ToolResultContent};
pub use usage::Usage;