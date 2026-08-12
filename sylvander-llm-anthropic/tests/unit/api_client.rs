use super::super::request::CreateMessageRequest;
use super::super::types::{MessageParam, OutputConfig};
use super::*;

#[test]
fn builder_requires_api_key() {
    let result = AnthropicClient::builder().build();
    match result {
        Err(AnthropicError::Validation(msg)) => {
            assert!(msg.contains("api_key"));
        }
        other => panic!("expected Validation error, got {other:?}"),
    }
}

#[test]
fn builder_validates_base_url() {
    let result = AnthropicClient::builder()
        .api_key("test-key")
        .base_url("not a url")
        .build();
    match result {
        Err(AnthropicError::Validation(msg)) => {
            assert!(msg.contains("base_url"));
        }
        other => panic!("expected Validation error, got {other:?}"),
    }
}

#[test]
fn builder_succeeds_with_api_key() {
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .expect("build should succeed");
    assert_eq!(client.base_url().as_str(), "https://api.anthropic.com/");
    assert_eq!(client.anthropic_version(), DEFAULT_ANTHROPIC_VERSION);
    assert!(client.beta_headers().is_empty());
}

#[test]
fn builder_accepts_custom_base_url() {
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .base_url("https://custom.example.com")
        .build()
        .expect("build should succeed");
    assert_eq!(client.base_url().as_str(), "https://custom.example.com/");
}

#[test]
fn builder_accepts_beta_headers() {
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .beta_header("prompt-caching-2024-07-31")
        .beta_header("extended-thinking-2025-01-01")
        .build()
        .expect("build should succeed");
    assert_eq!(client.beta_headers().len(), 2);
    assert_eq!(client.beta_headers()[0], "prompt-caching-2024-07-31");
}

#[test]
fn build_headers_includes_required_fields() {
    let client = AnthropicClient::builder()
        .api_key("sk-test-123")
        .build()
        .expect("build should succeed");
    let headers = client.build_headers();
    // Derived from anthropic-sdk-python@009b035
    // src/anthropic/_client.py::_api_key_auth.
    assert_eq!(headers.get("x-api-key").unwrap(), "sk-test-123");
    assert!(headers.get("authorization").is_none());
    assert_eq!(headers.get(CONTENT_TYPE).unwrap(), "application/json");
    assert_eq!(
        headers.get("anthropic-version").unwrap(),
        DEFAULT_ANTHROPIC_VERSION
    );
}

#[test]
fn build_headers_combines_beta_headers() {
    let client = AnthropicClient::builder()
        .api_key("sk-test")
        .beta_header("a")
        .beta_header("b")
        .build()
        .expect("build should succeed");
    let headers = client.build_headers();
    assert_eq!(headers.get("anthropic-beta").unwrap(), "a, b");
}

#[test]
fn stable_request_fields_do_not_invent_beta_headers() {
    let client = AnthropicClient::builder()
        .api_key("sk-test")
        .build()
        .expect("build should succeed");
    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(2048)
        .messages(vec![MessageParam::user("Hi")])
        .thinking(1024)
        .output_config(OutputConfig::default())
        .build()
        .unwrap();

    let headers = client.build_request_headers(&req);
    assert!(headers.get("anthropic-beta").is_none());
}

#[test]
fn build_request_headers_preserves_only_explicit_betas() {
    let client = AnthropicClient::builder()
        .api_key("sk-test")
        .beta_header("explicit-beta")
        .build()
        .expect("build should succeed");
    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Hi")])
        .output_config(OutputConfig::default())
        .build()
        .unwrap();

    let headers = client.build_request_headers(&req);
    assert_eq!(headers.get("anthropic-beta").unwrap(), "explicit-beta");
}

#[test]
fn build_request_headers_no_betas_when_request_plain() {
    let client = AnthropicClient::builder()
        .api_key("sk-test")
        .build()
        .expect("build should succeed");
    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Hi")])
        .build()
        .unwrap();

    let headers = client.build_request_headers(&req);
    assert!(headers.get("anthropic-beta").is_none());
}

#[test]
fn client_is_cloneable() {
    let client = AnthropicClient::builder()
        .api_key("sk-test")
        .build()
        .expect("build should succeed");
    let _cloned = client.clone();
}
