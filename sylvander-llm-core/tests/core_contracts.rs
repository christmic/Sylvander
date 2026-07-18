use std::collections::HashSet;
use std::sync::Arc;

use futures_util::{StreamExt, stream};
use serde_json::json;
use sylvander_llm_core::{
    CacheHint, ChatMessage, ChatRole, ContentBlock, DocumentContent, ImageContent, MediaSource,
    ModelCapabilities, ModelEventStream, ModelProvider, ModelRef, ModelRequest,
    ModelRequestCapabilityError, ModelRequestFeature, ModelResponse, ModelStreamEvent,
    OpaqueProviderState, ProviderErrorKind, ProviderFuture, ReasoningConfig,
    RequiredModelCapability, StopReason, SystemInstruction, TokenUsage, ToolDefinition,
    ToolResultContent, required_model_capabilities, validate_model_request_capabilities,
};

#[test]
fn provider_error_retryability_is_owned_by_neutral_classification() {
    for kind in [
        ProviderErrorKind::Transport,
        ProviderErrorKind::Timeout,
        ProviderErrorKind::RateLimited,
        ProviderErrorKind::Unavailable,
    ] {
        assert!(kind.is_retryable());
    }
    for kind in [
        ProviderErrorKind::Authentication,
        ProviderErrorKind::InvalidRequest,
        ProviderErrorKind::Protocol,
        ProviderErrorKind::Cancelled,
    ] {
        assert!(!kind.is_retryable());
    }
}

#[test]
fn provider_is_part_of_model_identity() {
    let mut models = HashSet::new();
    models.insert(ModelRef::new("anthropic", "shared-name"));
    models.insert(ModelRef::new("local", "shared-name"));
    assert_eq!(models.len(), 2);
}

struct FakeProvider;

impl ModelProvider for FakeProvider {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
        Box::pin(async move {
            let response = ModelResponse {
                id: request.request_id,
                model: request.model,
                content: Vec::new(),
                stop_reason: StopReason::EndTurn,
                usage: TokenUsage::default(),
            };
            let stream: ModelEventStream =
                Box::pin(stream::iter([Ok(ModelStreamEvent::Completed(response))]));
            Ok(stream)
        })
    }
}

#[tokio::test]
async fn provider_trait_is_object_safe_and_stream_is_owned() {
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider);
    let events = provider
        .complete_stream(base_request())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(
        events.as_slice(),
        [Ok(ModelStreamEvent::Completed(response))]
            if response.model == ModelRef::new("provider", "model")
    ));
    assert!(provider.model_catalog().await.unwrap().is_none());
}

#[test]
fn rich_request_and_response_round_trip_without_provider_wire_types() {
    let request = ModelRequest {
        request_id: "req-1".into(),
        model: ModelRef::new("provider-a", "model-a"),
        system: vec![SystemInstruction {
            text: "be precise".into(),
            cache_hint: Some(CacheHint::Ephemeral),
        }],
        messages: vec![
            ChatMessage::user("inspect this"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: vec![
                    ContentBlock::Reasoning {
                        text: "private reasoning".into(),
                        opaque_state: Some(OpaqueProviderState {
                            provider: "provider-a".into(),
                            data: json!({"signed": "opaque"}),
                        }),
                    },
                    ContentBlock::ToolCall {
                        id: "call-1".into(),
                        name: "read".into(),
                        arguments: json!({"path": "/tmp/a"}),
                    },
                ],
            },
            ChatMessage {
                role: ChatRole::User,
                content: vec![ContentBlock::ToolResult {
                    call_id: "call-1".into(),
                    content: vec![
                        ToolResultContent::Text {
                            text: "done".into(),
                        },
                        ToolResultContent::Image {
                            image: base64_image(),
                        },
                        ToolResultContent::Document {
                            document: url_document(),
                        },
                    ],
                    is_error: false,
                }],
            },
        ],
        tools: vec![ToolDefinition {
            name: "read".into(),
            description: "read a file".into(),
            input_schema: json!({"type": "object"}),
            cache_hint: Some(CacheHint::Ephemeral),
        }],
        max_output_tokens: 4096,
        reasoning: Some(ReasoningConfig { budget_tokens: 512 }),
        output_schema: Some(json!({"type": "object"})),
    };
    let request_json = serde_json::to_string(&request).unwrap();
    assert_eq!(
        serde_json::from_str::<ModelRequest>(&request_json).unwrap(),
        request
    );

    let event = ModelStreamEvent::Completed(ModelResponse {
        id: "message-1".into(),
        model: request.model.clone(),
        content: vec![
            ContentBlock::Text {
                text: "done".into(),
            },
            ContentBlock::Image {
                image: base64_image(),
            },
            ContentBlock::Document {
                document: url_document(),
            },
        ],
        stop_reason: StopReason::StopSequence("END".into()),
        usage: TokenUsage {
            input_tokens: 10,
            output_tokens: 2,
            cache_write_tokens: Some(3),
            cache_read_tokens: Some(4),
        },
    });
    let event_json = serde_json::to_string(&event).unwrap();
    assert_eq!(
        serde_json::from_str::<ModelStreamEvent>(&event_json).unwrap(),
        event
    );
}

#[test]
fn usage_accumulates_without_overflowing() {
    let mut total = TokenUsage {
        input_tokens: u64::MAX,
        output_tokens: 2,
        cache_write_tokens: None,
        cache_read_tokens: Some(u64::MAX),
    };
    total.saturating_add_assign(TokenUsage {
        input_tokens: 1,
        output_tokens: 5,
        cache_write_tokens: Some(7),
        cache_read_tokens: None,
    });
    assert_eq!(total.input_tokens, u64::MAX);
    assert_eq!(total.output_tokens, 7);
    assert_eq!(total.cache_write_tokens, Some(7));
    assert_eq!(total.cache_read_tokens, Some(u64::MAX));
    assert_eq!(total.total_input_tokens(), u64::MAX);

    let mut unknown = TokenUsage::default();
    unknown.saturating_add_assign(TokenUsage::default());
    assert_eq!(unknown.cache_write_tokens, None);
    assert_eq!(unknown.cache_read_tokens, None);
}

#[test]
fn request_features_require_all_six_capabilities_and_errors_are_redacted() {
    let mut input = base_request();
    input.tools.push(tool(None));
    input.reasoning = Some(ReasoningConfig { budget_tokens: 10 });
    input.output_schema = Some(json!({"secret-output-schema": true}));
    input.system.push(SystemInstruction {
        text: "secret-system".into(),
        cache_hint: Some(CacheHint::Ephemeral),
    });
    input.messages.push(ChatMessage {
        role: ChatRole::User,
        content: vec![
            ContentBlock::Image {
                image: secret_image(),
            },
            ContentBlock::Document {
                document: secret_document(),
            },
        ],
    });
    let tool_cap = ModelCapabilities::TOOL_USE;
    let reasoning = tool_cap | ModelCapabilities::REASONING;
    let schema = reasoning | ModelCapabilities::STRUCTURED_OUTPUT;
    let cache = schema | ModelCapabilities::PROMPT_CACHING;
    let vision = cache | ModelCapabilities::VISION;
    let all = vision | ModelCapabilities::DOCUMENT_INPUT;
    assert_eq!(required_model_capabilities(&input), all);
    for (available, capability, feature) in [
        (
            ModelCapabilities::empty(),
            RequiredModelCapability::ToolUse,
            ModelRequestFeature::ToolDefinitions,
        ),
        (
            tool_cap,
            RequiredModelCapability::Reasoning,
            ModelRequestFeature::ReasoningRequest,
        ),
        (
            reasoning,
            RequiredModelCapability::StructuredOutput,
            ModelRequestFeature::OutputSchema,
        ),
        (
            schema,
            RequiredModelCapability::PromptCaching,
            ModelRequestFeature::SystemCacheHint,
        ),
        (
            cache,
            RequiredModelCapability::Vision,
            ModelRequestFeature::DirectImage,
        ),
        (
            vision,
            RequiredModelCapability::DocumentInput,
            ModelRequestFeature::DirectDocument,
        ),
    ] {
        assert_missing(&input, available, capability, feature);
    }
    validate_model_request_capabilities(&input, all).unwrap();
    let rendered = format!(
        "{:?}",
        validate_model_request_capabilities(&input, ModelCapabilities::empty()).unwrap_err()
    );
    assert!(!rendered.contains("secret"));
}

#[test]
fn history_and_nested_media_stack_requirements() {
    let mut tool_history = base_request();
    tool_history.messages.push(ChatMessage {
        role: ChatRole::Assistant,
        content: vec![ContentBlock::ToolCall {
            id: "secret-call".into(),
            name: "secret-tool".into(),
            arguments: json!({}),
        }],
    });
    assert_missing(
        &tool_history,
        ModelCapabilities::empty(),
        RequiredModelCapability::ToolUse,
        ModelRequestFeature::ToolHistory,
    );

    let mut input = base_request();
    input.messages.push(ChatMessage {
        role: ChatRole::Assistant,
        content: vec![ContentBlock::Reasoning {
            text: "secret-reasoning".into(),
            opaque_state: None,
        }],
    });
    input.messages.push(ChatMessage {
        role: ChatRole::User,
        content: vec![ContentBlock::ToolResult {
            call_id: "secret-call".into(),
            content: vec![
                ToolResultContent::Image {
                    image: secret_image(),
                },
                ToolResultContent::Document {
                    document: secret_document(),
                },
            ],
            is_error: false,
        }],
    });
    let tool_cap = ModelCapabilities::TOOL_USE;
    let reasoning = tool_cap | ModelCapabilities::REASONING;
    let vision = reasoning | ModelCapabilities::VISION;
    assert_eq!(
        required_model_capabilities(&input),
        vision | ModelCapabilities::DOCUMENT_INPUT
    );
    for (available, capability, feature) in [
        (
            ModelCapabilities::empty(),
            RequiredModelCapability::ToolUse,
            ModelRequestFeature::ToolHistory,
        ),
        (
            tool_cap,
            RequiredModelCapability::Reasoning,
            ModelRequestFeature::ReasoningHistory,
        ),
        (
            reasoning,
            RequiredModelCapability::Vision,
            ModelRequestFeature::ToolResultImage,
        ),
        (
            vision,
            RequiredModelCapability::DocumentInput,
            ModelRequestFeature::ToolResultDocument,
        ),
    ] {
        assert_missing(&input, available, capability, feature);
    }
}

#[test]
fn tool_cache_hint_is_independently_gated() {
    let mut input = base_request();
    input.tools.push(tool(Some(CacheHint::Ephemeral)));
    assert_missing(
        &input,
        ModelCapabilities::TOOL_USE,
        RequiredModelCapability::PromptCaching,
        ModelRequestFeature::ToolCacheHint,
    );
}

fn base_request() -> ModelRequest {
    ModelRequest {
        request_id: "secret-request".into(),
        model: ModelRef::new("provider", "model"),
        system: vec![],
        messages: vec![ChatMessage::user("secret-message")],
        tools: vec![],
        max_output_tokens: 100,
        reasoning: None,
        output_schema: None,
    }
}

fn base64_image() -> ImageContent {
    ImageContent {
        source: MediaSource::Base64 {
            media_type: "image/png".into(),
            data: "cG5n".into(),
        },
        alt_text: Some("diagram".into()),
    }
}

fn url_document() -> DocumentContent {
    DocumentContent {
        source: MediaSource::Url {
            url: "https://example.invalid/spec.pdf".into(),
        },
        title: Some("spec".into()),
    }
}

fn secret_image() -> ImageContent {
    ImageContent {
        source: MediaSource::Url {
            url: "https://secret.invalid/image".into(),
        },
        alt_text: None,
    }
}

fn secret_document() -> DocumentContent {
    DocumentContent {
        source: MediaSource::Url {
            url: "https://secret.invalid/document".into(),
        },
        title: None,
    }
}

fn tool(cache_hint: Option<CacheHint>) -> ToolDefinition {
    ToolDefinition {
        name: "secret-tool".into(),
        description: "secret-description".into(),
        input_schema: json!({"secret-schema": true}),
        cache_hint,
    }
}

fn assert_missing(
    input: &ModelRequest,
    available: ModelCapabilities,
    capability: RequiredModelCapability,
    feature: ModelRequestFeature,
) {
    assert_eq!(
        validate_model_request_capabilities(input, available),
        Err(ModelRequestCapabilityError {
            capability,
            feature,
        })
    );
}
