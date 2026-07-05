//! `AgentEvent` — the reactive event stream emitted by [`AgentLoop`].
//!
//! The agent loop has a single core API — [`AgentLoop::run_stream`] —
//! that drives the iteration and yields events. [`AgentLoop::run`] is
//! a thin wrapper that consumes the stream and returns an
//! [`crate::AgentRun`]. [`AgentLoop::run_with_events`] is a wrapper
//! that fires events into a callback as they flow.
//!
//! Events fire in chronological order within a single iteration:
//! `IterationStart → TextChunk* / ThinkingChunk* → ToolCallStart →
//! ToolCallEnd → Compressed (optional) → IterationEnd → [repeat] → Done`

use serde_json::Value as JsonValue;

use sylvander_llm_anthropic::api::types::{Message, Usage};

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

    /// The model invoked a tool — about to execute it.
    ToolCallStart {
        /// Tool call ID (matches `tool_use.id`).
        id: String,
        /// Tool name.
        name: String,
        /// Parsed input arguments.
        input: JsonValue,
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

    /// Compression was applied this iteration.
    Compressed {
        /// Number of messages removed from the front.
        removed_count: usize,
        /// Estimated tokens freed (heuristic).
        freed_tokens: u32,
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
}