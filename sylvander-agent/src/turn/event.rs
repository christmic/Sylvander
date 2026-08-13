//! `AgentEvent` — the reactive event stream emitted by
//! [`AgentLoop`](crate::kernel::agent_loop::AgentLoop).
//!
//! The agent loop has a single core API — [`run_stream`](crate::kernel::agent_loop::run_stream) —
//! that drives the iteration and yields events. [`run`](crate::kernel::agent_loop::run) is
//! a thin wrapper that consumes the stream and returns an
//! [`AgentOutcome`]. [`run_with_events`](crate::kernel::agent_loop::run_with_events) is a wrapper
//! that fires events into a callback as they flow.
//!
//! Events fire in chronological order within a single iteration:
//! `IterationStart → Compressed (optional) → TextChunk* / ThinkingChunk* →
//! ToolCallStart → ToolCallEnd → IterationEnd → [repeat] → Done`

use serde_json::Value as JsonValue;

use sylvander_llm_core::{ChatMessage, TokenUsage};

use crate::context::compression::layer::LayerReport;
use crate::interaction::plan::PlanDecision;
use crate::tool::ToolFailureKind;
use crate::turn::error::AgentLoopError;
use crate::turn::machine::TurnTransition;
use crate::turn::outcome::AgentOutcome;

/// Provider-neutral reason for retrying a model request.
///
/// The Agent owns retry semantics because retry is part of its execution state
/// machine. Runtime translates this value to a public API event; keeping the
/// wire enum out of this contract prevents UI protocol versions from changing
/// kernel behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRetryCause {
    RateLimit,
    Server,
    Network,
    Stream,
    Other,
}

/// Events emitted by the agent loop. All consumption paths
/// (`run()`, `run_with_events()`, `run_stream()`) consume the same
/// underlying stream — there is one source of truth for the iteration.
#[derive(Debug)]
pub enum AgentEvent {
    /// Authoritative content-free transition of the current turn machine.
    TurnTransition(TurnTransition),

    /// A new iteration is starting (LLM call about to fire).
    IterationStart {
        /// Iteration number, 1-indexed.
        iteration: u32,
    },

    /// Incremental text from the model's response. Multiple per
    /// iteration when streaming.
    TextChunk(String),

    /// Incremental thinking content (when extended thinking enabled).
    ThinkingChunk(String),

    /// A transient model failure will be retried after a bounded backoff.
    ModelRetry {
        /// Retry number about to run, 1-indexed.
        attempt: u32,
        /// Maximum retries configured for this request phase.
        max_attempts: u32,
        /// Backoff delay before the retry starts.
        delay_ms: u64,
        /// Sanitized provider error suitable for diagnostics and UI.
        reason: String,
        cause: ModelRetryCause,
    },

    /// A provider tool call was parsed and is about to enter approval.
    ///
    /// Runtime uses this content-free identity boundary for durable lifecycle
    /// admission before any approval or execution side effect.
    ToolCallPrepared {
        id: String,
        name: String,
    },

    /// An approved tool call is about to execute.
    ToolCallStart {
        /// Tool call ID (matches `tool_use.id`).
        id: String,
        /// Tool name.
        name: String,
        /// Parsed input arguments.
        input: JsonValue,
    },

    /// Incremental output produced while a tool call is still running.
    ToolCallOutputDelta {
        id: String,
        name: String,
        delta: String,
    },

    ToolTimedOut {
        id: String,
        name: String,
        timeout_secs: u64,
    },

    /// Tool execution finished.
    ToolCallEnd {
        /// Tool call ID.
        id: String,
        /// Tool name.
        name: String,
        /// Tool output (success or `is_error: true` content).
        output: String,
        /// `true` if the tool returned `is_error: true`.
        is_error: bool,
        /// Trusted classification supplied by the executor, independent from
        /// model-visible error text.
        failure_kind: Option<ToolFailureKind>,
    },

    /// Tool execution was rejected by the approval gate (not executed).
    ToolRejected {
        /// Tool call ID.
        id: String,
        /// Tool name.
        name: String,
        /// Rejection reason.
        reason: String,
    },

    /// An LLM-backed automatic compaction is about to start.
    CompressionStarted,

    /// Compression was applied this iteration.
    ///
    /// Always emitted when at least one layer produced work (removed,
    /// condensed, freed tokens, or recorded a failure). For pipelines
    /// this is a `Vec<LayerReport>` with one entry per layer that ran.
    /// For the legacy single-strategy path it is a 1-element vec.
    Compressed {
        /// Per-layer breakdown. Empty only if no layer ran.
        layers: Vec<LayerReport>,
    },
    /// Internal synchronization snapshot emitted immediately after
    /// `Compressed`; consumers that only need telemetry can ignore it.
    HistoryCompacted {
        /// Exact history that the next provider request will receive.
        /// Runtime's `AgentRun` uses this to keep subsequent turns in sync.
        history: Vec<ChatMessage>,
        layers: Vec<LayerReport>,
    },

    /// An iteration completed (LLM call returned). The next iteration
    /// may start, or the loop may end.
    IterationEnd {
        /// Iteration number that just completed.
        iteration: u32,
        /// Cumulative usage so far.
        usage: TokenUsage,
        /// Usage reported by this provider request only. Consumers use this
        /// for context-window tracking and incremental durable accounting.
        provider_usage: TokenUsage,
    },

    /// The loop terminated successfully with its complete commit candidate.
    ///
    /// Runtime persists the returned conversation and usage; intermediate
    /// stream events are not authoritative storage records.
    Done(AgentOutcome),

    /// The loop terminated with an error.
    Error(AgentLoopError),

    /// Model is asking the user a question. Loop is paused (M18).
    AskUser {
        call_id: String,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    },

    /// User answered an `AskUser` question (M18).
    UserAnswer {
        call_id: String,
        answer: Vec<String>,
    },
    PlanProposed {
        plan_id: String,
        steps: Vec<String>,
    },
    PlanResolved {
        plan_id: String,
        decision: PlanDecision,
    },
}
