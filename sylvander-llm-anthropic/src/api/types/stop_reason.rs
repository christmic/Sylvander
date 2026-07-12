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
mod tests {
    use super::*;

    #[test]
    fn serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&StopReason::EndTurn).unwrap(),
            r#""end_turn""#
        );
        assert_eq!(
            serde_json::to_string(&StopReason::MaxTokens).unwrap(),
            r#""max_tokens""#
        );
        assert_eq!(
            serde_json::to_string(&StopReason::StopSequence).unwrap(),
            r#""stop_sequence""#
        );
        assert_eq!(
            serde_json::to_string(&StopReason::ToolUse).unwrap(),
            r#""tool_use""#
        );
    }

    #[test]
    fn deserializes_snake_case() {
        assert_eq!(
            serde_json::from_str::<StopReason>(r#""end_turn""#).unwrap(),
            StopReason::EndTurn
        );
        assert_eq!(
            serde_json::from_str::<StopReason>(r#""pause_turn""#).unwrap(),
            StopReason::PauseTurn
        );
        assert_eq!(
            serde_json::from_str::<StopReason>(r#""refusal""#).unwrap(),
            StopReason::Refusal
        );
    }

    #[test]
    fn unknown_variant_falls_into_other() {
        // MiniMax-M3 returns "abort" for context-overflow cases.
        // We catch it as Other instead of failing the entire
        // response deserialization.
        let r: StopReason = serde_json::from_str(r#""abort""#).unwrap();
        assert_eq!(r, StopReason::Other);
        assert!(r.is_terminal());
    }

    #[test]
    fn roundtrip_all_variants() {
        for reason in [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::StopSequence,
            StopReason::ToolUse,
            StopReason::PauseTurn,
            StopReason::Refusal,
        ] {
            let s = serde_json::to_string(&reason).unwrap();
            let back: StopReason = serde_json::from_str(&s).unwrap();
            assert_eq!(back, reason);
        }
    }

    #[test]
    fn is_terminal_classification() {
        assert!(StopReason::EndTurn.is_terminal());
        assert!(StopReason::StopSequence.is_terminal());
        assert!(StopReason::Refusal.is_terminal());
        assert!(StopReason::Other.is_terminal());
        assert!(!StopReason::ToolUse.is_terminal());
        assert!(!StopReason::MaxTokens.is_terminal());
        assert!(!StopReason::PauseTurn.is_terminal());
    }
}
