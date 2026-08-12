use super::*;

#[test]
fn serializes_minimal() {
    let usage = Usage {
        input_tokens: 100,
        output_tokens: 50,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        ..Usage::default()
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
        ..Usage::default()
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
        ..Usage::default()
    };
    assert_eq!(usage.total_input_tokens(), 5220);
}

#[test]
fn official_sdk_usage_shape_preserves_every_reported_dimension() {
    // Derived from anthropic-sdk-python@009b035:
    // src/anthropic/types/{usage,cache_creation,output_tokens_details,
    // server_tool_usage}.py.
    let json = r#"{
        "cache_creation": {
            "ephemeral_1h_input_tokens": 11,
            "ephemeral_5m_input_tokens": 22
        },
        "cache_creation_input_tokens": 33,
        "cache_read_input_tokens": 44,
        "inference_geo": "us",
        "input_tokens": 55,
        "output_tokens": 66,
        "output_tokens_details": {"thinking_tokens": 7},
        "server_tool_use": {"web_fetch_requests": 2, "web_search_requests": 3},
        "service_tier": "priority"
    }"#;
    let usage: Usage = serde_json::from_str(json).unwrap();

    assert_eq!(usage.cache_creation.unwrap().ephemeral_1h_input_tokens, 11);
    assert_eq!(usage.cache_creation.unwrap().ephemeral_5m_input_tokens, 22);
    assert_eq!(usage.output_tokens_details.unwrap().thinking_tokens, 7);
    assert_eq!(usage.server_tool_use.unwrap().web_fetch_requests, 2);
    assert_eq!(usage.server_tool_use.unwrap().web_search_requests, 3);
    assert_eq!(usage.inference_geo.as_deref(), Some("us"));
    assert_eq!(usage.service_tier, Some(ServiceTier::Priority));
}
