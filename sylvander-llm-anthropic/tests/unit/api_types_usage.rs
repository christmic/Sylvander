use super::*;

#[test]
fn serializes_minimal() {
    let usage = Usage {
        input_tokens: 100,
        output_tokens: 50,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    };
    assert_eq!(
        serde_json::to_string(&usage).unwrap(),
        r#"{"input_tokens":100,"output_tokens":50}"#
    );
}

#[test]
fn serializes_full() {
    let usage = Usage {
        input_tokens: 100,
        output_tokens: 50,
        cache_creation_input_tokens: Some(1024),
        cache_read_input_tokens: Some(4096),
    };
    let json = serde_json::to_string(&usage).unwrap();
    let back: Usage = serde_json::from_str(&json).unwrap();
    assert_eq!(back, usage);
}

#[test]
fn deserializes_minimal_from_anthropic() {
    let json = r#"{"input_tokens":42,"output_tokens":7}"#;
    let usage: Usage = serde_json::from_str(json).unwrap();
    assert_eq!(usage.input_tokens, 42);
    assert_eq!(usage.output_tokens, 7);
    assert_eq!(usage.cache_creation_input_tokens, None);
    assert_eq!(usage.cache_read_input_tokens, None);
}

#[test]
fn total_input_tokens_sums_all() {
    let usage = Usage {
        input_tokens: 100,
        output_tokens: 50,
        cache_creation_input_tokens: Some(1024),
        cache_read_input_tokens: Some(4096),
    };
    assert_eq!(usage.total_input_tokens(), 5220);
}
