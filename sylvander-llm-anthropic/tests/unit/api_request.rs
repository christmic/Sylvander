use super::*;
use crate::api::error::AnthropicError;
use crate::api::types::{InputSchema, Tool};

#[test]
fn builder_minimal_required() {
    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Hi")])
        .build()
        .expect("build should succeed");
    assert_eq!(req.model, "claude-sonnet-5-20260601");
    assert_eq!(req.max_tokens, 1024);
    assert_eq!(req.messages.len(), 1);
}

#[test]
fn builder_missing_model_errors() {
    let result = CreateMessageRequest::builder()
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Hi")])
        .build();
    assert!(matches!(result, Err(AnthropicError::Validation(_))));
}

#[test]
fn builder_missing_max_tokens_errors() {
    let result = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .messages(vec![MessageParam::user("Hi")])
        .build();
    assert!(matches!(result, Err(AnthropicError::Validation(_))));
}

#[test]
fn builder_missing_messages_errors() {
    let result = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .build();
    assert!(matches!(result, Err(AnthropicError::Validation(_))));
}

#[test]
fn validate_empty_messages_errors() {
    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![])
        .build()
        .unwrap();
    assert!(req.validate().is_err());
}

#[test]
fn validate_temperature_out_of_range_errors() {
    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Hi")])
        .temperature(1.5)
        .build()
        .unwrap();
    assert!(req.validate().is_err());
}

#[test]
fn official_sdk_thinking_budget_bounds_are_enforced() {
    // Derived from anthropic-sdk-python@009b035
    // src/anthropic/types/thinking_config_enabled_param.py.
    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(2048)
        .messages(vec![MessageParam::user("Hi")])
        .thinking(2048)
        .build()
        .unwrap();
    assert!(req.validate().is_err());

    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(2048)
        .messages(vec![MessageParam::user("Hi")])
        .thinking(1023)
        .build()
        .unwrap();
    assert!(req.validate().is_err());
}

#[test]
fn builder_with_tools() {
    let tool = Tool::new("ping", "Health check", InputSchema::empty());
    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Ping")])
        .tool(tool)
        .build()
        .unwrap();
    assert_eq!(req.tools.len(), 1);
    assert_eq!(req.tools[0].name, "ping");
}

#[test]
fn serialization_omits_optional_fields() {
    let req = CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Hi")])
        .build()
        .unwrap();
    let json = serde_json::to_string(&req).unwrap();
    assert!(!json.contains("system"));
    assert!(!json.contains("tools"));
    assert!(!json.contains("temperature"));
}
