//! # sylvander-agent
//!
//! Sylvander Agent execution core — an asynchronous reactive driver that
//! calls a selected model provider, executes governed tools, persists turn
//! state, re-feeds results, and emits typed events as work progresses.
//!
//! ## Scope
//!
//! - Provider-neutral model execution with an Anthropic wire adapter
//! - Reactive event stream (`AgentEvent` + `run_stream()`)
//! - Governed built-in, MCP, and embedding-supplied tools
//! - Durable sessions, typed prompt/context composition, and memory
//! - Multi-layer context compression and bounded tool-result handling
//! - Retry, cancellation, approval, capability, and iteration controls
//!
//! ## Quickstart
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use sylvander_agent::{
//!     prelude::{AgentLoop, MessageParam, ToolContext},
//!     tool_context::Cap,
//! };
//! use sylvander_llm_anthropic::{
//!     AnthropicProvider,
//!     api::{
//!         client::AnthropicClient,
//!         model::{ModelCapabilities, ModelInfo},
//!     },
//! };
//! use sylvander_llm_core::{
//!     ModelCapabilities as ProviderCapabilities, ModelInfo as ProviderModelInfo, ModelRef,
//! };
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
//! let exact_model = ProviderModelInfo {
//!     reference: ModelRef::new("anthropic", model.id.clone()),
//!     context_window: model.context_window,
//!     max_output_tokens: model.max_output_tokens,
//!     capabilities: ProviderCapabilities::TOOL_USE,
//! };
//!
//! let loop_ = AgentLoop::builder()
//!     .qualified_router(Arc::new(AnthropicProvider::new("anthropic", client)))
//!     .provider_model(exact_model)
//!     .tool_context(
//!         ToolContext::new(sylvander_protocol::SessionContext::new(
//!             "user", "agent", "session",
//!         ))
//!         .with_fs_root("/tmp")
//!         .with_capability(Cap::Read),
//!     )
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
//!         2. Build and validate one provider-neutral request
//!         3. Stream through the exact qualified route with bounded retry
//!         4. emit events (TextChunk, ThinkingChunk, ...)
//!         5. Re-feed assistant message
//!         6. stop_reason match:
//!            EndTurn / StopSequence / MaxTokens → break (Done)
//!            ToolUse → execute tools, build tool_result, re-feed
//!     }
//! }
//! ```
//!
//! The crate-level design and ownership boundaries are documented in
//! `sylvander-agent/docs/ARCHITECTURE.md`.

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
/// Runtime-owned Guardian candidate and curated-context contracts.
pub mod curated_memory;
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
/// Internal translation between Anthropic wire types and provider-neutral
/// model contracts. This is a current adapter, not a fallback backend.
mod provider_adapter;
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
/// Central actor-aware authorization and audit contract for tool execution.
pub mod tool_invocation;
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

#[cfg(test)]
#[path = "../tests/unit/support.rs"]
pub(crate) mod test_support;

/// Convenient re-exports for the most commonly used types.
/// Populated as each module lands in subsequent commits.
pub mod prelude {
    pub use crate::bus::{
        AgentStatus, BusError, BusMessage, InProcessMessageBus, MessageBus, MessageId, MessageKind,
        Recipient, Sender, StreamEvent, SubscriptionFilter, SystemMessage,
    };
    pub use crate::compress::{
        AutoCompactLlm, CompressContext, DEFAULT_SUMMARY_PROMPT,
        layer::{
            CompressionLayer, LayerReport, first_failure, total_condensed, total_freed,
            total_removed,
        },
        pipeline::CompressionPipeline,
    };
    pub use crate::curated_memory::{
        CuratedContextEntry, CuratedContextProvider, CuratedContextSubject, CuratedMemoryScope,
        MemoryCandidateError, MemoryCandidateReceipt, MemoryCandidateSink,
        MemoryCandidateSubmission,
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
    pub use crate::tool::{Tool, ToolError, ToolOutput, ToolProgressSink, ToolRegistry};
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
