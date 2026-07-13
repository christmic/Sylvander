//! `AgentEvent` — the reactive event stream emitted by [`AgentLoop`].
//!
//! The agent loop has a single core API — [`AgentLoop::run_stream`] —
//! that drives the iteration and yields events. [`AgentLoop::run`] is
//! a thin wrapper that consumes the stream and returns an
//! [`crate::AgentLoopResult`]. [`AgentLoop::run_with_events`] is a wrapper
//! that fires events into a callback as they flow.
//!
//! Events fire in chronological order within a single iteration:
//! `IterationStart → Compressed (optional) → TextChunk* / ThinkingChunk* →
//! ToolCallStart → ToolCallEnd → IterationEnd → [repeat] → Done`

use serde_json::Value as JsonValue;

use sylvander_llm_anthropic::api::types::{Message, MessageParam, Usage};

use crate::compress::layer::LayerReport;
use crate::error::AgentLoopError;

/// Events emitted by the agent loop. All consumption paths
/// (`run()`, `run_with_events()`, `run_stream()`) consume the same
/// underlying stream — there is one source of truth for the iteration.
#[derive(Debug)]
pub enum AgentEvent {
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
    },

    /// The model invoked a tool — about to execute it.
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
        /// AgentRun uses this to keep subsequent turns in sync.
        history: Vec<MessageParam>,
        layers: Vec<LayerReport>,
    },

    /// An iteration completed (LLM call returned). The next iteration
    /// may start, or the loop may end.
    IterationEnd {
        /// Iteration number that just completed.
        iteration: u32,
        /// Cumulative usage so far.
        usage: Usage,
    },

    /// The loop has terminated successfully (model emitted `end_turn`
    /// or hit `max_iterations` without `end_turn`).
    Done(Message),

    /// The loop terminated with an error.
    Error(AgentLoopError),

    /// Model is asking the user a question. Loop is paused (M18).
    AskUser {
        call_id: String,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    },

    /// User answered an AskUser question (M18).
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
        decision: sylvander_protocol::PlanDecision,
    },
}
