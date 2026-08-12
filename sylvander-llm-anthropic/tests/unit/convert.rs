use super::*;

#[test]
fn rich_neutral_request_maps_to_anthropic_wire() {
    let input = core::ModelRequest {
        request_id: "req-1".into(),
        model: core::ModelRef::new("anthropic", "claude-test"),
        system: vec![core::SystemInstruction {
            text: "system".into(),
            cache_hint: Some(core::CacheHint::Ephemeral),
        }],
        messages: vec![
            core::ChatMessage::user("hello"),
            core::ChatMessage {
                role: core::ChatRole::Assistant,
                content: vec![
                    core::ContentBlock::Reasoning {
                        text: "think".into(),
                        opaque_state: Some(core::OpaqueProviderState {
                            provider: "anthropic".into(),
                            data: json!({"signature": "signed"}),
                        }),
                    },
                    core::ContentBlock::ToolCall {
                        id: "call-1".into(),
                        name: "read".into(),
                        arguments: json!({"path": "/tmp/a"}),
                    },
                ],
            },
            core::ChatMessage {
                role: core::ChatRole::User,
                content: vec![core::ContentBlock::ToolResult {
                    call_id: "call-1".into(),
                    content: vec![core::ToolResultContent::Document {
                        document: core::DocumentContent {
                            source: core::MediaSource::Url {
                                url: "https://example.invalid/a.pdf".into(),
                            },
                            title: Some("a".into()),
                        },
                    }],
                    is_error: false,
                }],
            },
        ],
        tools: vec![core::ToolDefinition {
            name: "read".into(),
            description: "read".into(),
            input_schema: json!({"type": "object"}),
            cache_hint: Some(core::CacheHint::Ephemeral),
        }],
        max_output_tokens: 2048,
        reasoning: Some(core::ReasoningConfig {
            budget_tokens: Some(1024),
            effort: None,
        }),
        output_schema: Some(json!({"type": "object"})),
    };
    let mapped = request(&input).unwrap();
    let encoded = serde_json::to_value(mapped).unwrap();
    assert_eq!(encoded["model"], "claude-test");
    assert_eq!(encoded["messages"][1]["content"][0]["signature"], "signed");
    assert_eq!(encoded["messages"][2]["content"][0]["type"], "tool_result");
    assert_eq!(encoded["thinking"]["type"], "enabled");
    assert_eq!(encoded["thinking"]["budget_tokens"], 1024);
    assert_eq!(encoded["output_config"]["format"]["type"], "json_schema");
}

#[test]
fn errors_are_neutral_and_status_aware() {
    const SENSITIVE: &str = "secret-upstream-body-marker";
    let mapped = error(
        AnthropicError::Api {
            status: 429,
            error_type: "rate_limit".into(),
            error_message: SENSITIVE.into(),
            request_id: Some("upstream-1".into()),
        },
        core::ProviderErrorPhase::Open,
    );
    assert_eq!(mapped.kind, core::ProviderErrorKind::RateLimited);
    assert_eq!(mapped.status, Some(429));
    assert_eq!(mapped.request_id.as_deref(), Some("upstream-1"));
    assert!(mapped.is_retryable());
    assert!(!mapped.message.contains(SENSITIVE));
    assert!(!mapped.to_string().contains(SENSITIVE));
    assert!(!format!("{mapped:?}").contains(SENSITIVE));

    let protocol = error(
        AnthropicError::SseParse {
            message: "bad event".into(),
            position: 3,
        },
        core::ProviderErrorPhase::Stream,
    );
    assert_eq!(protocol.kind, core::ProviderErrorKind::Protocol);
    assert!(!protocol.is_retryable());
}

#[test]
fn response_preserves_content_stop_reason_and_usage() {
    let mapped = response(
        "anthropic",
        wire::Message {
            id: "msg-1".into(),
            kind: wire::MessageKind::Message,
            role: wire::MessageRole::Assistant,
            content: vec![
                wire::ContentBlock::Text(wire::TextBlock::new("answer")),
                wire::ContentBlock::Thinking(wire::ThinkingBlock::new("think", "signed")),
                wire::ContentBlock::ToolUse(wire::ToolUseBlock::new(
                    "call-1",
                    "read",
                    json!({"path": "/tmp/a"}),
                )),
                wire::ContentBlock::RedactedThinking(wire::RedactedThinkingBlock::new(
                    "opaque-data",
                )),
            ],
            model: "claude-test".into(),
            stop_reason: Some(wire::StopReason::StopSequence),
            stop_sequence: Some("END".into()),
            usage: wire::Usage {
                input_tokens: 10,
                output_tokens: 2,
                cache_creation_input_tokens: Some(3),
                cache_read_input_tokens: Some(4),
                cache_creation: Some(wire::CacheCreation {
                    ephemeral_1h_input_tokens: 1,
                    ephemeral_5m_input_tokens: 2,
                }),
                output_tokens_details: Some(wire::OutputTokensDetails { thinking_tokens: 1 }),
                ..wire::Usage::default()
            },
        },
    );
    assert_eq!(
        mapped.model,
        core::ModelRef::new("anthropic", "claude-test")
    );
    assert_eq!(
        mapped.stop_reason,
        core::StopReason::StopSequence("END".into())
    );
    assert_eq!(mapped.usage.total_input_tokens(), 17);
    assert_eq!(mapped.usage.cache_write_tokens, Some(3));
    assert_eq!(mapped.usage.cache_read_tokens, Some(4));
    assert_eq!(mapped.usage.details.cache_write_1h_tokens, Some(1));
    assert_eq!(mapped.usage.details.cache_write_5m_tokens, Some(2));
    assert_eq!(mapped.usage.details.reasoning_tokens, Some(1));
    assert!(matches!(
        &mapped.content[1],
        core::ContentBlock::Reasoning { opaque_state: Some(state), .. }
            if state.data["signature"] == "signed"
    ));
    assert!(
        matches!(&mapped.content[2], core::ContentBlock::ToolCall { name, .. } if name == "read")
    );
    assert!(matches!(
        &mapped.content[3],
        core::ContentBlock::Reasoning { text, opaque_state: Some(state) }
            if text.is_empty() && state.data["redacted_thinking"] == "opaque-data"
    ));
}

#[test]
fn redacted_thinking_is_refed_without_modification() {
    let message = core::ChatMessage {
        role: core::ChatRole::Assistant,
        content: vec![core::ContentBlock::Reasoning {
            text: String::new(),
            opaque_state: Some(core::OpaqueProviderState {
                provider: "anthropic".into(),
                data: json!({"redacted_thinking": "opaque-data"}),
            }),
        }],
    };

    let mapped = super::message(&message).unwrap();
    let encoded = serde_json::to_value(mapped).unwrap();
    assert_eq!(encoded["content"][0]["type"], "redacted_thinking");
    assert_eq!(encoded["content"][0]["data"], "opaque-data");
}

#[test]
fn official_adaptive_thinking_and_effort_shape_is_preserved() {
    let mut input = core::ModelRequest {
        request_id: "req-adaptive".into(),
        model: core::ModelRef::new("anthropic", "claude-opus-4-6"),
        system: Vec::new(),
        messages: vec![core::ChatMessage::user("hello")],
        tools: Vec::new(),
        max_output_tokens: 1024,
        reasoning: Some(core::ReasoningConfig {
            budget_tokens: None,
            effort: Some(core::ReasoningEffort::Low),
        }),
        output_schema: Some(json!({"type": "object"})),
    };
    let encoded = serde_json::to_value(request(&input).unwrap()).unwrap();
    assert_eq!(encoded["thinking"], json!({"type": "adaptive"}));
    assert_eq!(encoded["output_config"]["effort"], "low");
    assert_eq!(encoded["output_config"]["format"]["type"], "json_schema");

    input.reasoning = Some(core::ReasoningConfig {
        budget_tokens: None,
        effort: Some(core::ReasoningEffort::Disabled),
    });
    input.output_schema = None;
    let encoded = serde_json::to_value(request(&input).unwrap()).unwrap();
    assert_eq!(encoded["thinking"], json!({"type": "disabled"}));
    assert!(encoded.get("output_config").is_none());
}
