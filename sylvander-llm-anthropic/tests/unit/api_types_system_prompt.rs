use super::*;

#[test]
fn string_system_round_trip() {
    let sp: SystemPrompt = "You are a helpful assistant.".into();
    let json = serde_json::to_string(&sp).unwrap();
    assert_eq!(json, r#""You are a helpful assistant.""#);
    let back: SystemPrompt = serde_json::from_str(&json).unwrap();
    assert_eq!(back, sp);
}

#[test]
fn blocks_system_round_trip() {
    let sp = SystemPrompt::Blocks(vec![SystemBlock::Text(
        SystemTextBlock::new("You are a helpful assistant.")
            .with_cache_control(CacheControl::ephemeral()),
    )]);
    let json = serde_json::to_string(&sp).unwrap();
    assert!(json.contains(r#""type":"text""#));
    assert!(json.contains(r#""cache_control":"#));
    let back: SystemPrompt = serde_json::from_str(&json).unwrap();
    assert_eq!(back, sp);
}
