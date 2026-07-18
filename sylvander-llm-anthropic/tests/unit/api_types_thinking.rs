use super::*;

#[test]
fn serializes_minimal() {
    let tc = ThinkingConfig::new(1024);
    assert_eq!(
        serde_json::to_string(&tc).unwrap(),
        r#"{"budget_tokens":1024}"#
    );
}

#[test]
fn roundtrip() {
    let tc = ThinkingConfig::new(8192);
    let json = serde_json::to_string(&tc).unwrap();
    let back: ThinkingConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back, tc);
}
