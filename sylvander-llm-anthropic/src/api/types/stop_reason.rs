//! Stop reason — why the model finished generating.

use serde::{Deserialize, Serialize};

/// Reason the model stopped generating.
///
/// Wire format is `snake_case`. See
/// [Anthropic docs](https://platform.claude.com/docs/en/build-with-claude/handling-stop-reasons)
/// for the meaning of each variant.
///
/// Some providers (and future Anthropic API versions) may emit
/// stop reasons we don't recognize — e.g. MiniMax-M3 returns
/// `"abort"` for context-length overflows. We catch these in
/// [`StopReason::Other`] (a unit variant) so the response still
/// deserializes. The original string is not preserved; treat
/// `Other` as terminal and log the raw response if you need to
/// distinguish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Model reached a natural stopping point (end of message).
    EndTurn,
    /// `max_tokens` was reached or the model's natural limit.
    MaxTokens,
    /// A custom `stop_sequence` was generated.
    StopSequence,
    /// The model invoked one or more tools.
    ToolUse,
    /// A long-running turn was paused; can be continued by feeding the
    /// response back in.
    PauseTurn,
    /// Streaming classifier intervened to handle a potential policy
    /// violation.
    Refusal,
    /// Unrecognized stop reason (e.g. `"abort"` from MiniMax-M3).
    /// Treat as terminal — log the raw response body for debugging.
    #[serde(other)]
    Other,
}

impl StopReason {
    /// True if the model emitted a recognized "natural end of turn"
    /// signal — `EndTurn`, `StopSequence`, `Refusal`, or an
    /// unknown reason. False if the model is mid-flight (`ToolUse`,
    /// `PauseTurn`) or hit a hard cap (`MaxTokens`).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            StopReason::EndTurn
                | StopReason::StopSequence
                | StopReason::Refusal
                | StopReason::Other
        )
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/api_types_stop_reason.rs"]
mod tests;
