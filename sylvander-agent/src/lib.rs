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

/// Prompt composition, retrieved context, profiles, and compression.
pub mod context;
/// Runtime-injected execution capabilities for one bounded Agent turn.
pub mod execution;
/// User decisions and bounded background-work boundaries for one turn.
pub mod interaction;
/// Provider-neutral execution policy and model/tool iteration state machine.
pub mod kernel;
/// Relationship-memory domain, retention rules, and Runtime-owned ports.
pub mod memory;
/// Immutable input, authority, progress, and result vocabulary for one turn.
pub mod turn;

// Public facade for the established crate API. Internal code uses `turn::*`
// paths so the physical ownership remains visible during development.
pub use context::{
    compression as compress, prompt, turn_context, user_profile, user_profile_prompt,
    user_profile_provider,
};
pub use execution::{
    artifact, mutation_journal as workspace_journal, ports as execution_ports, risk, tool_context,
    workspace as workspace_executor,
};
pub use interaction::{
    approval, ask_user as ask_user_gate, background_task as task_gate, plan as plan_gate,
};
pub use kernel::agent_loop as loop_;
pub use memory::curated as curated_memory;
pub use turn::{
    conversation, error, event, execution_context, identity, machine, outcome, request, time,
};
/// Tool contracts, authorization, registration, and built-in implementations.
pub mod tool;
pub use tool::{builtins as tools, invocation as tool_invocation};

#[cfg(test)]
#[path = "../tests/unit/support.rs"]
pub(crate) mod test_support;
#[cfg(test)]
#[path = "../tests/unit/support_workspace.rs"]
pub(crate) mod test_workspace;

/// Convenient re-exports for the most commonly used types.
/// Populated as each module lands in subsequent commits.
pub mod prelude {
    pub use crate::context::compression::{
        AutoCompactLlm, CompressContext, DEFAULT_SUMMARY_PROMPT,
        layer::{
            CompressionLayer, LayerReport, first_failure, total_condensed, total_freed,
            total_removed,
        },
        pipeline::CompressionPipeline,
    };
    pub use crate::context::turn_context::{
        TurnContextBudget, TurnContextBudgets, TurnContextLayerKind, TurnContextManifest,
    };
    pub use crate::execution::artifact::{
        ArtifactReference, ArtifactStoreError, ArtifactWrite, TurnArtifactStore,
    };
    pub use crate::execution::ports::AgentExecutionPorts;
    pub use crate::execution::risk::{CommandRiskAssessment, CommandRiskLevel, CommandRiskReason};
    pub use crate::execution::tool_context::ToolContext;
    pub use crate::execution::workspace::{
        ProcessIsolation, WorkspaceCommandOutput, WorkspaceCommandProgressSink,
        WorkspaceCommandStream, WorkspaceEntryKind, WorkspaceExecutor, WorkspaceExecutorError,
        WorkspaceListEntry, WorkspaceListRequest, WorkspaceListResult, WorkspacePolicyViolation,
        WorkspaceQueryLimits, WorkspaceSearchMatch, WorkspaceSearchRequest, WorkspaceSearchResult,
        WorkspaceTarget,
    };
    pub use crate::interaction::plan::PlanDecision;
    pub use crate::kernel::agent_loop::{
        AgentLoop, AgentLoopBuilder, run, run_stream, run_with_events,
    };
    pub use crate::memory::curated::{
        CuratedContextEntry, CuratedContextProvider, CuratedContextSubject, CuratedMemoryScope,
        MemoryCandidateError, MemoryCandidateReceipt, MemoryCandidateSink,
        MemoryCandidateSubmission,
    };
    pub use crate::memory::store::{
        InMemoryMemoryStore, MemoryActorKind, MemoryAppend, MemoryEntry, MemoryExecutionContext,
        MemoryExpiryPatch, MemoryOwner, MemoryPatch, MemoryProvenance, MemoryProvenanceSource,
        MemoryScope, MemoryStore, MemoryStoreError, RelationshipMemoryRetentionPolicy,
    };
    pub use crate::tool::builtins::{
        EditTool, ListTool, MemoryReadTool, MemoryWriteTool, PresentPlanTool, ReadTool, SearchTool,
        StartBackgroundTaskTool, UpdatePlanTool, WriteTool,
    };
    pub use crate::tool::{
        AgentHookPhase, PreparedToolCall, RegisteredTool, SandboxRequirement, ToolDefinition,
        ToolEnvironmentError, ToolError, ToolExecutionMode, ToolExecutionPolicy, ToolExecutor,
        ToolExposure, ToolFailureKind, ToolFilesystemPolicy, ToolNetworkPolicy, ToolOutput,
        ToolPreparation, ToolPrepareError, ToolProgressSink, ToolRegistry, ToolSourceFeature,
        ToolSourceKind, ToolSourceStatus, ToolSpec,
    };
    pub use crate::turn::conversation::ConversationSnapshot;
    pub use crate::turn::error::AgentLoopError;
    pub use crate::turn::event::{AgentEvent, ModelRetryCause};
    pub use crate::turn::execution_context::{
        AgentExecutionContext, ExecutionActor, ExecutionCapability, ExecutionWorkspace,
    };
    pub use crate::turn::machine::{
        TurnContinuationReason, TurnPhase, TurnSnapshot, TurnStateError, TurnTransition,
        TurnTransitionReason,
    };
    pub use crate::turn::outcome::AgentOutcome;
    pub use crate::turn::request::AgentTurnRequest;
    pub use sylvander_llm_core::{
        ChatMessage, ChatRole, ContentBlock, InputSchema, ModelResponse, StopReason, TokenUsage,
    };
}
