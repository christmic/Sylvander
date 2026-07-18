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
