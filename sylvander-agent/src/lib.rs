//! # sylvander-agent
//!
//! Sylvander Agent execution core — an asynchronous reactive driver that
//! calls a selected model provider, executes governed tools, re-feeds results,
//! and emits typed events as work progresses.
//!
//! ## Scope
//!
//! - Provider-neutral model execution through `sylvander-llm-core`
//! - Reactive event stream (`AgentEvent` + `run_stream()`)
//! - Governed built-in, MCP, and embedding-supplied tools
//! - Immutable turn requests and Runtime-supplied execution ports
//! - Multi-layer context compression and bounded tool-result handling
//! - Retry, cancellation, approval, capability, and iteration controls
//!
//! ## Quickstart
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use sylvander_agent::{
//!     prelude::{
//!         AgentExecutionContext, AgentExecutionPorts, AgentLoop, AgentTurnRequest,
//!         ChatMessage, ConversationSnapshot, ToolContext, ToolRegistry,
//!     },
//!     tool_invocation::{RegistryBoundToolGateway, ToolInvocationGateway as _},
//! };
//! use sylvander_llm_core::{
//!     ModelCapabilities, ModelInfo, ModelProvider, ModelRef,
//! };
//!
//! # fn build_provider() -> Arc<dyn ModelProvider> { unimplemented!() }
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let exact_model = ModelInfo {
//!     reference: ModelRef::new("configured-provider", "selected-model"),
//!     context_window: 200_000,
//!     max_output_tokens: 32_000,
//!     capabilities: ModelCapabilities::TOOL_USE,
//! };
//!
//! let kernel = AgentLoop::builder().max_iterations(50).build();
//! let execution = AgentExecutionContext::restricted_for("user", "agent", "execution");
//! let tools = ToolRegistry::new();
//! let gateway = RegistryBoundToolGateway::new(tools.invocation_descriptors());
//! let request = AgentTurnRequest {
//!     conversation: ConversationSnapshot::new(vec![ChatMessage::user("Say hello")]),
//!     model: exact_model,
//!     system_instructions: Vec::new(),
//!     reasoning: None,
//!     tools,
//!     execution: execution.clone(),
//! };
//! let ports = AgentExecutionPorts::new(
//!     build_provider(),
//!     ToolContext::new(execution),
//!     gateway.clone(),
//!     gateway.snapshot(),
//! );
//!
//! // Await full completion
//! let run = sylvander_agent::prelude::run(&kernel, request, ports).await?;
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
/// Model-visible conversation snapshot owned only for one execution.
pub mod conversation;
/// Runtime-owned Guardian candidate and curated-context contracts.
pub mod curated_memory;
/// Per-session Agent run scheduling and lifecycle ownership.
pub mod engine;
/// Agent-loop error taxonomy.
pub mod error;
/// Fine-grained loop events for observers and tests.
pub mod event;
/// Trusted, non-wire authority for one Agent execution.
pub mod execution_context;
pub mod execution_ports;
/// Provider-compatible iterative model/tool execution loop.
pub mod loop_;
/// Managed MCP stdio client, discovery, and tool adapter.
pub mod mcp_stdio;
/// Provider-neutral result returned to the Runtime owner.
pub mod outcome;
/// Plan proposal and acknowledgement gate.
pub mod plan_gate;
/// Deterministic system-prompt composition.
pub mod prompt;
/// Immutable input for one Agent execution.
pub mod request;
/// Internal translation between Anthropic wire types and provider-neutral
/// model contracts. This is a current adapter, not a fallback backend.
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
    pub use crate::conversation::ConversationSnapshot;
    pub use crate::curated_memory::{
        CuratedContextEntry, CuratedContextProvider, CuratedContextSubject, CuratedMemoryScope,
        MemoryCandidateError, MemoryCandidateReceipt, MemoryCandidateSink,
        MemoryCandidateSubmission,
    };
    pub use crate::engine::{AgentHandle, AgentRunEngine, EngineError, SessionMeta};
    pub use crate::error::AgentLoopError;
    pub use crate::event::AgentEvent;
    pub use crate::execution_context::{
        AgentExecutionContext, ExecutionActor, ExecutionCapability, ExecutionWorkspace,
    };
    pub use crate::execution_ports::AgentExecutionPorts;
    pub use crate::loop_::{AgentLoop, AgentLoopBuilder, run, run_stream, run_with_events};
    pub use crate::mcp_stdio::{McpError, McpStdioClient, McpTool};
    pub use crate::outcome::AgentOutcome;
    pub use crate::request::AgentTurnRequest;
    pub use crate::run::{
        AgentRun, AgentRunBuilder, AgentRunError, AgentSessionIssuer, AuthenticatedSession,
        AuthenticatedSessionLease,
    };
    pub use crate::session::{SessionContext, SessionMetadata};
    pub use crate::spec::{
        AgentId, AgentSpec, AgentSpecBuilder, BehaviorConfig, McpServerConfig, MemoryStoreConfig,
        ModelConfig, PersonaConfig, SessionId, ToolRef,
    };
    pub use crate::tool::{
        PreparedToolCall, RegisteredTool, SandboxRequirement, ToolDefinition, ToolEnvironmentError,
        ToolError, ToolExecutionMode, ToolExecutionPolicy, ToolExecutor, ToolExposure,
        ToolFilesystemPolicy, ToolNetworkPolicy, ToolOutput, ToolPreparation, ToolPrepareError,
        ToolProgressSink, ToolRegistry, ToolSpec,
    };
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
        LocalExecutor, ProcessIsolation, WorkspaceCommandOutput, WorkspaceCommandProgressSink,
        WorkspaceCommandStream, WorkspaceEntryKind, WorkspaceExecutor, WorkspaceExecutorError,
        WorkspaceListEntry, WorkspaceListRequest, WorkspaceListResult, WorkspaceQueryLimits,
        WorkspaceSearchMatch, WorkspaceSearchRequest, WorkspaceSearchResult, WorkspaceTarget,
    };
    pub use sylvander_llm_core::{
        ChatMessage, ChatRole, ContentBlock, InputSchema, ModelResponse, StopReason, TokenUsage,
    };
    /// Compatibility name for callers migrating from protocol-specific messages.
    pub type MessageParam = ChatMessage;
    pub use sylvander_protocol::types::UserId;
}
