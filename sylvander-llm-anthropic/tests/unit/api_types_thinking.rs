use super::*;

#[test]
fn serializes_minimal() {
    let tc = ThinkingConfig::new(1024);
    assert_eq!(
        serde_json::to_string(&tc).unwrap(),
        r#"{"type":"enabled","budget_tokens":1024}"#
    );
}

#[test]
fn official_sdk_adaptive_and_display_shapes() {
    // Derived from anthropic-sdk-python@009b035:
    // src/anthropic/types/thinking_config_{adaptive,enabled}_param.py.
    assert_eq!(
        serde_json::to_value(ThinkingConfig::adaptive()).unwrap(),
        serde_json::json!({"type": "adaptive"})
    );
    assert_eq!(
        serde_json::to_value(ThinkingConfig::new(2048).with_display(ThinkingDisplay::Omitted))
            .unwrap(),
        serde_json::json!({
            "type": "enabled",
            "budget_tokens": 2048,
            "display": "omitted"
        })
    );
}

#[test]
fn roundtrip() {
    let tc = ThinkingConfig::new(8192);
    let json = serde_json::to_string(&tc).unwrap();
    let back: ThinkingConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back, tc);
}
