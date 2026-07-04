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
//! | [`thinking`] | Extended thinking config |
//! | [`tool`] | Custom function tools + tool choice |
//! | [`tool_result`] | `ToolResultBlock` (user re-feed of tool output) |
//! | [`usage`] | Token usage accounting |

pub mod block;
pub mod cache;
pub mod image;
pub mod message;
pub mod output_config;
pub mod stop_reason;
pub mod thinking;
pub mod tool;
pub mod tool_result;
pub mod usage;

pub use block::{ContentBlock, ToolUseBlock, UserContent, UserContentBlock};
pub use cache::{CacheControl, CacheControlKind, CacheTtl};
pub use image::{Base64ImageSource, ImageBlock, ImageMediaType, ImageSource};
pub use message::{Message, MessageKind, MessageParam, MessageRole, MessageTokensCount};
pub use output_config::{Effort, JsonOutputFormat, JsonOutputFormatKind, OutputConfig};
pub use stop_reason::StopReason;
pub use thinking::ThinkingConfig;
pub use tool::{InputSchema, Tool, ToolChoice};
pub use tool_result::{RichToolResultBlock, ToolResultBlock, ToolResultContent};
pub use usage::Usage;