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
//! let loop_ = AgentLoop::builder()
//!     .client(client)
//!     .model(model)
//!     .max_iterations(50)
//!     .build()?;
//!
//! let initial = vec![MessageParam::user("List files in /tmp")];
//!
//! // Await full completion
//! let run = sylvander_agent::prelude::run(&loop_, initial).await?;
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

pub mod approval;
pub mod ask_user_gate;
pub mod bus;
pub mod compress;
pub mod engine;
pub mod error;
pub mod event;
pub mod loop_;
pub mod plan_gate;
pub mod provider_compat;
pub mod run;
pub mod session;
pub mod session_store;
pub mod spec;
pub mod task_gate;
pub mod tool;
pub mod tool_context;
pub mod tools;
pub mod workspace_journal;

/// Convenient re-exports for the most commonly used types.
/// Populated as each module lands in subsequent commits.
pub mod prelude {
    pub use crate::bus::{
        AgentStatus, BusError, BusMessage, InProcessMessageBus, MessageBus, MessageId, MessageKind,
        Recipient, Sender, StreamEvent, SubscriptionFilter, SystemMessage,
    };
    pub use crate::compress::{
        AgentLoopAutoCompactLlm, AutoCompactLlm, CompressContext, DEFAULT_SUMMARY_PROMPT,
        layer::{
            CompressionLayer, LayerReport, first_failure, total_condensed, total_freed,
            total_removed,
        },
        pipeline::CompressionPipeline,
    };
    pub use crate::engine::{AgentHandle, AgentRunEngine, EngineError, SessionMeta};
    pub use crate::error::AgentLoopError;
    pub use crate::event::AgentEvent;
    pub use crate::loop_::{
        AgentLoop, AgentLoopBuilder, AgentLoopResult, run, run_stream, run_with_events,
    };
    pub use crate::run::{AgentRun, AgentRunBuilder, AgentRunError};
    pub use crate::session::{SessionContext, SessionMetadata};
    pub use crate::spec::{
        AgentId, AgentSpec, AgentSpecBuilder, BehaviorConfig, McpServerConfig, MemoryStoreConfig,
        ModelConfig, PersonaConfig, SessionId, ToolRef,
    };
    pub use crate::tool::{MockTool, Tool, ToolError, ToolOutput, ToolProgressSink, ToolRegistry};
    pub use crate::tool_context::ToolContext;
    pub use crate::tools::{
        EditTool, InMemoryMemoryStore, MemoryEntry, MemoryReadTool, MemoryStore, MemoryStoreError,
        MemoryWriteTool, PresentPlanTool, ReadTool, StartBackgroundTaskTool, UpdatePlanTool,
        WriteTool,
    };
    pub use sylvander_llm_anthropic::prelude::*;
    pub use sylvander_protocol::types::UserId;
}
