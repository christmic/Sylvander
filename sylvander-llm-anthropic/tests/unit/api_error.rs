use super::*;

#[test]
fn api_4xx_is_not_retryable() {
    let err = AnthropicError::Api {
        status: 400,
        error_type: "invalid_request_error".into(),
        error_message: "bad input".into(),
        request_id: Some("req_abc".into()),
    };
    assert!(!err.is_retryable());
    assert_eq!(err.status(), Some(400));
    assert_eq!(err.request_id(), Some("req_abc"));
}

#[test]
fn api_429_is_retryable() {
    let err = AnthropicError::Api {
        status: 429,
        error_type: "rate_limit_error".into(),
        error_message: "slow down".into(),
        request_id: None,
    };
    assert!(err.is_retryable());
    assert_eq!(err.status(), Some(429));
}

#[test]
fn api_5xx_is_retryable() {
    for status in [500u16, 502, 503, 504, 529] {
        let err = AnthropicError::Api {
            status,
            error_type: "api_error".into(),
            error_message: "transient".into(),
            request_id: None,
        };
        assert!(err.is_retryable(), "status {status} should be retryable");
        assert_eq!(err.status(), Some(status));
    }
}

#[test]
fn json_error_is_not_retryable() {
    // Json error variant has no status
    let result: Result<serde_json::Value, _> = serde_json::from_str("{invalid");
    let err: AnthropicError = result.unwrap_err().into();
    assert!(!err.is_retryable());
    assert_eq!(err.status(), None);
    assert_eq!(err.request_id(), None);
}

#[test]
fn sse_parse_is_retryable() {
    let err = AnthropicError::SseParse {
        message: "incomplete event".into(),
        position: 42,
    };
    assert!(err.is_retryable());
    assert_eq!(err.status(), None);
}

#[test]
fn validation_error_is_not_retryable() {
    let err = AnthropicError::Validation("max_tokens must be > 0".into());
    assert!(!err.is_retryable());
    assert_eq!(err.status(), None);
}

#[test]
fn unknown_block_type_is_not_retryable() {
    let err = AnthropicError::UnknownBlockType("weird_type".into());
    assert!(!err.is_retryable());
}

#[test]
fn unknown_stream_event_type_is_not_retryable() {
    let err = AnthropicError::UnknownStreamEventType("mystery_event".into());
    assert!(!err.is_retryable());
}

#[test]
fn display_messages_are_informative() {
    let err = AnthropicError::Api {
        status: 400,
        error_type: "invalid_request_error".into(),
        error_message: "model is required".into(),
        request_id: Some("req_xyz".into()),
    };
    let msg = format!("{err}");
    assert!(msg.contains("400"));
    assert!(msg.contains("invalid_request_error"));
    assert!(msg.contains("model is required"));
}
