//! Stop reason — why the model finished generating.

use serde::{Deserialize, Serialize};

/// Reason the model stopped generating.
///
/// Wire format is `snake_case`. See
/// [Anthropic docs](https://platform.claude.com/docs/en/build-with-claude/handling-stop-reasons)
/// for the meaning of each variant.
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
}