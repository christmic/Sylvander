use super::*;

#[test]
fn parse_api_error_with_full_body() {
    let body =
        br#"{"type":"invalid_request_error","message":"model is required","request_id":"req_abc"}"#;
    let err = parse_api_error(400, body);
    match err {
        AnthropicError::Api {
            status,
            error_type,
            error_message,
            request_id,
        } => {
            assert_eq!(status, 400);
            assert_eq!(error_type, "invalid_request_error");
            assert_eq!(error_message, "model is required");
            assert_eq!(request_id.as_deref(), Some("req_abc"));
        }
        other => panic!("expected Api variant, got {other:?}"),
    }
}

#[test]
fn parse_api_error_with_minimal_body() {
    let body = br#"{"type":"rate_limit_error","message":"slow down"}"#;
    let err = parse_api_error(429, body);
    match err {
        AnthropicError::Api {
            status,
            error_type,
            error_message,
            request_id,
        } => {
            assert_eq!(status, 429);
            assert_eq!(error_type, "rate_limit_error");
            assert_eq!(error_message, "slow down");
            assert!(request_id.is_none());
        }
        other => panic!("expected Api variant, got {other:?}"),
    }
}

#[test]
fn parse_api_error_with_garbage_body() {
    let body = b"not json at all";
    let err = parse_api_error(500, body);
    match err {
        AnthropicError::Api {
            status,
            error_type,
            error_message,
            ..
        } => {
            assert_eq!(status, 500);
            assert_eq!(error_type, "unparseable");
            assert!(error_message.contains("not json"));
        }
        other => panic!("expected Api variant, got {other:?}"),
    }
}

#[test]
fn endpoint_url_appends_path() {
    let base = Url::parse("https://api.anthropic.com/").unwrap();
    let url = MessagesApi::endpoint_url(&base, "v1/messages/count_tokens").unwrap();
    assert_eq!(
        url.as_str(),
        "https://api.anthropic.com/v1/messages/count_tokens"
    );
}
