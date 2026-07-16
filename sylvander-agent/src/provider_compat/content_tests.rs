use super::*;

fn image() -> core::ImageContent {
    core::ImageContent {
        source: core::MediaSource::Base64 {
            media_type: "image/png".into(),
            data: "cG5n".into(),
        },
        alt_text: None,
    }
}

fn response(provider: &str, content: Vec<core::ContentBlock>) -> core::ModelResponse {
    core::ModelResponse {
        id: "msg".into(),
        model: core::ModelRef::new(provider, "model"),
        content,
        stop_reason: core::StopReason::EndTurn,
        usage: core::TokenUsage {
            input_tokens: 1,
            output_tokens: 2,
            cache_write_tokens: Some(0),
            cache_read_tokens: None,
        },
    }
}

#[test]
fn message_table_round_trips_supported_content_and_rejects_document() {
    let messages = [
        core::ChatMessage {
            role: core::ChatRole::Assistant,
            content: vec![
                core::ContentBlock::Reasoning {
                    text: "think".into(),
                    opaque_state: Some(core::OpaqueProviderState {
                        provider: ANTHROPIC.into(),
                        data: json!({"signature": "sig"}),
                    }),
                },
                core::ContentBlock::ToolCall {
                    id: "call".into(),
                    name: "read".into(),
                    arguments: json!({"path": "a"}),
                },
            ],
        },
        core::ChatMessage {
            role: core::ChatRole::User,
            content: vec![core::ContentBlock::ToolResult {
                call_id: "call".into(),
                content: vec![
                    core::ToolResultContent::Text { text: "ok".into() },
                    core::ToolResultContent::Image { image: image() },
                ],
                is_error: false,
            }],
        },
    ];
    for message in messages {
        assert_eq!(
            message_to_core(&message_from_core(&message).unwrap()).unwrap(),
            message
        );
    }
    let document = core::ChatMessage {
        role: core::ChatRole::User,
        content: vec![core::ContentBlock::Document {
            document: core::DocumentContent {
                source: core::MediaSource::Url { url: "x".into() },
                title: None,
            },
        }],
    };
    assert!(message_from_core(&document).is_err());
}

#[test]
fn response_table_preserves_stop_usage_and_provider_boundary() {
    for (reason, expected, sequence) in [
        (core::StopReason::EndTurn, legacy::StopReason::EndTurn, None),
        (core::StopReason::ToolUse, legacy::StopReason::ToolUse, None),
        (
            core::StopReason::MaxOutputTokens,
            legacy::StopReason::MaxTokens,
            None,
        ),
        (core::StopReason::Refusal, legacy::StopReason::Refusal, None),
        (
            core::StopReason::Paused,
            legacy::StopReason::PauseTurn,
            None,
        ),
        (
            core::StopReason::StopSequence("END".into()),
            legacy::StopReason::StopSequence,
            Some("END"),
        ),
    ] {
        let mut core = response(
            "openai",
            vec![core::ContentBlock::Text { text: "ok".into() }],
        );
        core.stop_reason = reason;
        let message = response_from_core(core).unwrap();
        assert_eq!(message.stop_reason, Some(expected));
        assert_eq!(message.stop_sequence.as_deref(), sequence);
        assert_eq!(message.usage.cache_creation_input_tokens, Some(0));
        assert_eq!(message.usage.cache_read_input_tokens, None);
    }
    let reasoning = core::ContentBlock::Reasoning {
        text: "think".into(),
        opaque_state: Some(core::OpaqueProviderState {
            provider: "openai".into(),
            data: json!({"signature": "not-anthropic"}),
        }),
    };
    assert!(matches!(
        response_from_core(response("openai", vec![reasoning])),
        Err(ProviderCompatError::ProviderMismatch { .. })
    ));
}
