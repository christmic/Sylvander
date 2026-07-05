//! # sylvander-agent
//!
//! Sylvander v2 Agent Loop — async reactive driver that calls the
//! Anthropic Messages API, executes tools, re-feeds results, and emits
//! events as the loop progresses.
//!
//! ## Scope (M2)
//!
//! - `AgentLoop` struct with builder pattern (OOP class-based)
//! - Reactive event stream (`AgentEvent` + `run_stream()`)
//! - `Tool` trait + `ToolRegistry` (caller plugs in their own tools)
//! - `Compressor` trait + simple default impl
//! - Retry / backoff + capability validation + iteration limit
//! - **No concrete tools** (Read/Bash/Edit) — those land in M3
//!
//! ## Quickstart
//!
//! ```no_run
//! use sylvander_llm_anthropic::prelude::*;
//! use sylvander_agent::prelude::*;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! // Caller builds their own model registry (per C11 architecture).
//! let model = ModelInfo::builder()
//!     .id("claude-sonnet-5-20260601")
//!     .context_window(200_000)
//!     .max_output_tokens(32_000)
//!     .capability(ModelCapabilities::TOOL_USE)
//!     .build()
//!     .unwrap();
//!
//! let client = AnthropicClient::builder()
//!     .api_key(std::env::var("ANTHROPIC_API_KEY")?)
//!     .build()?;
//!
//! let mut loop_ = AgentLoop::builder()
//!     .client(client)
//!     .model(model)
//!     .max_iterations(50)
//!     .build()?;
//!
//! let initial = vec![MessageParam::user("List files in /tmp")];
//!
//! // Await full completion
//! let run = loop_.run(initial).await?;
//! println!("finished after {} iterations", run.iterations);
//! # Ok(())
//! # }
//! ```
//!
//! ## Architecture
//!
//! ```text
//! run() {
//!     for iteration in 1..=max_iterations {
//!         1. Compressor.maybe_compress(&mut messages, ...)
//!         2. Validate capabilities (tools / thinking / cache_ttl)
//!         3. call_with_retry(client.messages().create, 3)
//!         4. emit events (TextChunk, ThinkingChunk, ...)
//!         5. Re-feed assistant message
//!         6. stop_reason match:
//!            EndTurn / StopSequence / MaxTokens → break (Done)
//!            ToolUse → execute tools, build tool_result, re-feed
//!     }
//! }
//! ```
//!
//! ## References
//!
//! - `projects/Sylvander/designs/m1-m2-m3-roadmap.md` — M2 scope
//! - `projects/Sylvander/designs/sylvander-llm-anthropic-design.md`
//!   — M1 design notes (the protocol layer this loop drives)
//! - `projects/Sylvander/designs/anthropic-sdk-capabilities.md` —
//!   capability analysis

#![doc(html_root_url = "https://docs.rs/sylvander-agent/0.1.0")]

pub mod compress;
pub mod error;
pub mod event;
pub mod loop_;
pub mod tool;

/// Convenient re-exports for the most commonly used types.
/// Populated as each module lands in subsequent commits.
pub mod prelude {
    pub use crate::compress::{
        CompressContext, CompressionOutcome, Compressor, NoCompression,
        SimpleWindowCompressor,
    };
    pub use crate::error::AgentLoopError;
    pub use crate::event::AgentEvent;
    pub use crate::loop_::{AgentLoop, AgentLoopBuilder, AgentRun};
    pub use crate::tool::{MockTool, Tool, ToolError, ToolOutput, ToolRegistry};
    pub use sylvander_llm_anthropic::prelude::*;
}