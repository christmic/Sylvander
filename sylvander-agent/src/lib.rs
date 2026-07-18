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

/// Approval request persistence, policy evaluation, and user decisions.
pub mod approval;
mod approval_store;
/// One-shot AskUser prompt/answer gate for an Agent run.
pub mod ask_user_gate;
/// In-process message bus, stream events, and subscription filtering.
pub mod bus;
/// Context-window compaction contracts and pipeline implementations.
pub mod compress;
/// Per-session Agent run scheduling and lifecycle ownership.
pub mod engine;
/// Agent-loop error taxonomy.
pub mod error;
/// Fine-grained loop events for observers and tests.
pub mod event;
/// Provider-compatible iterative model/tool execution loop.
pub mod loop_;
/// Managed MCP stdio client, discovery, and tool adapter.
pub mod mcp_stdio;
/// Plan proposal and acknowledgement gate.
pub mod plan_gate;
/// Deterministic system-prompt composition.
pub mod prompt;
/// Compatibility translation between legacy Anthropic and core provider types.
pub mod provider_compat;
/// Authenticated single-turn execution and durable transcript handling.
pub mod run;
/// Session context and runtime metadata carried by an Agent run.
pub mod session;
/// Durable session/transcript persistence contracts and SQLite implementation.
pub mod session_store;
/// Declarative Agent identity, model, tool, and workspace specification.
pub mod spec;
/// Restricted background-task lifecycle and result gate.
pub mod task_gate;
/// Tool registration, schemas, invocation, and normalized output.
pub mod tool;
/// Runtime-derived capability, identity, workspace, and execution budget context.
pub mod tool_context;
/// Built-in filesystem, memory, plan, and task tools.
pub mod tools;
/// Typed, budgeted, provenance-preserving context for one authenticated turn.
pub mod turn_context;
/// Bounded prompt layer generated from a user profile.
pub mod user_profile_prompt;
/// Runtime abstraction for retrieving authorized user profiles.
pub mod user_profile_provider;
/// Location-neutral filesystem and command execution contract.
pub mod workspace_executor;
/// Durable workspace-change journal used for review and recovery.
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
    pub use crate::mcp_stdio::{McpError, McpStdioClient, McpTool};
    pub use crate::run::{
        AgentRun, AgentRunBuilder, AgentRunError, AgentSessionIssuer, AuthenticatedSession,
        AuthenticatedSessionLease,
    };
    pub use crate::session::{SessionContext, SessionMetadata};
    pub use crate::spec::{
        AgentId, AgentSpec, AgentSpecBuilder, BehaviorConfig, McpServerConfig, MemoryStoreConfig,
        ModelConfig, PersonaConfig, SessionId, ToolRef,
    };
    pub use crate::tool::{MockTool, Tool, ToolError, ToolOutput, ToolProgressSink, ToolRegistry};
    pub use crate::tool_context::ToolContext;
    pub use crate::tools::{
        EditTool, InMemoryMemoryStore, ListTool, MemoryActorKind, MemoryAppend,
        MemoryBackupArtifact, MemoryBackupManifest, MemoryClock, MemoryEntry,
        MemoryEvidenceCheckpoint, MemoryEvidenceCompactionReport, MemoryExecutionContext,
        MemoryExpiryPatch, MemoryIntegrityConfig, MemoryOwner, MemoryPatch, MemoryProvenance,
        MemoryProvenanceSource, MemoryPurgeReport, MemoryReadTool, MemoryRestoreError, MemoryScope,
        MemoryStore, MemoryStoreError, MemoryWriteTool, PresentPlanTool, ReadTool,
        RelationshipMemoryRetentionPolicy, SearchTool, SqliteMemoryAdmin, SqliteMemoryMaintenance,
        SqliteMemoryStore, StartBackgroundTaskTool, SystemMemoryClock, UpdatePlanTool, WriteTool,
    };
    pub use crate::turn_context::{
        TurnContextBudget, TurnContextBudgets, TurnContextLayerKind, TurnContextManifest,
    };
    pub use crate::workspace_executor::{
        LocalExecutor, WorkspaceCommandOutput, WorkspaceCommandProgressSink,
        WorkspaceCommandStream, WorkspaceEntryKind, WorkspaceExecutor, WorkspaceExecutorError,
        WorkspaceListEntry, WorkspaceListRequest, WorkspaceListResult, WorkspaceQueryLimits,
        WorkspaceSearchMatch, WorkspaceSearchRequest, WorkspaceSearchResult, WorkspaceTarget,
    };
    pub use sylvander_llm_anthropic::prelude::*;
    pub use sylvander_protocol::types::UserId;
}
