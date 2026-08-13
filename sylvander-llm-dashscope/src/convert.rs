//! Conversion boundary between neutral calls and native Generation wire types.

use sylvander_llm_core::{
    ChatMessage, ChatRole, ContentBlock, ModelRef, ModelRequest, ModelResponse, ProviderError,
    ProviderErrorKind, ProviderErrorPhase, ReasoningEffort, StopReason, TokenUsage,
    TokenUsageDetails, ToolResultContent,
};

use crate::DashScopeFeatures;
use crate::api::{
    DashScopeError, GenerationCompletion, GenerationFunctionCallParam,
    GenerationFunctionDefinition, GenerationFunctionTool, GenerationInput, GenerationMessageParam,
    GenerationParameters, GenerationRequest, GenerationToolCallParam, GenerationToolKind,
    GenerationUsage, MultimodalContent, MultimodalGenerationInput, MultimodalGenerationRequest,
    MultimodalMessageParam,
};

pub(crate) fn request(
    features: &DashScopeFeatures,
    input: &ModelRequest,
) -> Result<GenerationRequest, ProviderError> {
    if input.output_schema.is_some() {
        return Err(unsupported(
            "DashScope Generation json_object mode cannot enforce a JSON Schema",
        ));
    }
    let mut messages = input
        .system
        .iter()
        .map(|instruction| GenerationMessageParam::System {
            content: instruction.text.clone(),
        })
        .collect::<Vec<_>>();
    for message in &input.messages {
        append_message(message, features, &mut messages)?;
    }
    let (enable_thinking, thinking_budget) = reasoning(features, input)?;
    let tools = input
        .tools
        .iter()
        .map(|tool| GenerationFunctionTool {
            kind: GenerationToolKind::Function,
            function: GenerationFunctionDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            },
        })
        .collect::<Vec<_>>();
    Ok(GenerationRequest {
        model: input.model.model.clone(),
        input: GenerationInput { messages },
        parameters: GenerationParameters {
            result_format: "message".into(),
            incremental_output: true,
            max_tokens: input.max_output_tokens,
            enable_thinking,
            thinking_budget,
            parallel_tool_calls: (features.contains("parallel_tool_calls") && !tools.is_empty())
                .then_some(true),
            tools,
        },
    })
}

pub(crate) fn multimodal_request(
    features: &DashScopeFeatures,
    input: &ModelRequest,
) -> Result<MultimodalGenerationRequest, ProviderError> {
    if input.output_schema.is_some() || !input.tools.is_empty() {
        return Err(unsupported(
            "DashScope Multimodal Generation does not expose this request feature",
        ));
    }
    let mut messages = input
        .system
        .iter()
        .map(|instruction| MultimodalMessageParam {
            role: "system".into(),
            content: vec![MultimodalContent::Text {
                text: instruction.text.clone(),
            }],
        })
        .collect::<Vec<_>>();
    for message in &input.messages {
        if message.role != ChatRole::User {
            return Err(unsupported(
                "DashScope Multimodal Generation benchmark supports user input only",
            ));
        }
        let mut content = Vec::new();
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => {
                    content.push(MultimodalContent::Text { text: text.clone() });
                }
                ContentBlock::Image { image } => {
                    let image = match &image.source {
                        sylvander_llm_core::MediaSource::Url { url } => url.clone(),
                        sylvander_llm_core::MediaSource::Base64 { media_type, data } => {
                            format!("data:{media_type};base64,{data}")
                        }
                    };
                    content.push(MultimodalContent::Image { image });
                }
                ContentBlock::Document { .. }
                | ContentBlock::Reasoning { .. }
                | ContentBlock::ToolCall { .. }
                | ContentBlock::ToolResult { .. } => {
                    return Err(unsupported(
                        "DashScope Multimodal Generation input block is unsupported",
                    ));
                }
            }
        }
        messages.push(MultimodalMessageParam {
            role: "user".into(),
            content,
        });
    }
    let (enable_thinking, thinking_budget) = reasoning(features, input)?;
    Ok(MultimodalGenerationRequest {
        model: input.model.model.clone(),
        input: MultimodalGenerationInput { messages },
        parameters: GenerationParameters {
            result_format: "message".into(),
            incremental_output: true,
            max_tokens: input.max_output_tokens,
            enable_thinking,
            thinking_budget,
            parallel_tool_calls: None,
            tools: Vec::new(),
        },
    })
}

fn reasoning(
    features: &DashScopeFeatures,
    input: &ModelRequest,
) -> Result<(Option<bool>, Option<u32>), ProviderError> {
    let Some(reasoning) = input.reasoning else {
        return Ok((None, None));
    };
    if matches!(reasoning.effort, Some(ReasoningEffort::Disabled)) {
        return Ok((features.contains("enable_thinking").then_some(false), None));
    }
    if !features.contains("enable_thinking") {
        return Err(unsupported(
            "DashScope reasoning requires enable_thinking support",
        ));
    }
    if reasoning.budget_tokens.is_none() && reasoning.effort.is_some() {
        return Err(unsupported(
            "DashScope Generation cannot represent qualitative reasoning effort",
        ));
    }
    let budget = match reasoning.budget_tokens {
        Some(value) if features.contains("thinking_budget") => Some(value),
        Some(_) => {
            return Err(unsupported(
                "DashScope reasoning budget requires thinking_budget support",
            ));
        }
        None => None,
    };
    Ok((Some(true), budget))
}

fn append_message(
    message: &ChatMessage,
    features: &DashScopeFeatures,
    output: &mut Vec<GenerationMessageParam>,
) -> Result<(), ProviderError> {
    let tool_results = message
        .content
        .iter()
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .count();
    if tool_results > 0 {
        if message.role != ChatRole::User || tool_results != message.content.len() {
            return Err(invalid(
                "DashScope requires tool results in dedicated user messages",
            ));
        }
        for block in &message.content {
            let ContentBlock::ToolResult {
                call_id, content, ..
            } = block
            else {
                unreachable!("validated tool-result-only message")
            };
            output.push(GenerationMessageParam::Tool {
                tool_call_id: call_id.clone(),
                content: result_text(content)?,
            });
        }
        return Ok(());
    }
    match message.role {
        ChatRole::User => append_user(message, output),
        ChatRole::Assistant => append_assistant(message, features, output),
    }
}

fn append_user(
    message: &ChatMessage,
    output: &mut Vec<GenerationMessageParam>,
) -> Result<(), ProviderError> {
    let mut text = String::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: value } => text.push_str(value),
            ContentBlock::Image { .. } => {
                return Err(unsupported(
                    "images use DashScope MultiModalConversation, not Generation",
                ));
            }
            ContentBlock::Document { .. } => {
                return Err(unsupported(
                    "documents are not native Generation message content",
                ));
            }
            ContentBlock::Reasoning { .. } | ContentBlock::ToolCall { .. } => {
                return Err(invalid("DashScope user message contains assistant content"));
            }
            ContentBlock::ToolResult { .. } => unreachable!("handled above"),
        }
    }
    output.push(GenerationMessageParam::User { content: text });
    Ok(())
}

fn append_assistant(
    message: &ChatMessage,
    features: &DashScopeFeatures,
    output: &mut Vec<GenerationMessageParam>,
) -> Result<(), ProviderError> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tools = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: value } => text.push_str(value),
            ContentBlock::Reasoning { text: value, .. }
                if features.contains("reasoning_content") =>
            {
                reasoning.push_str(value);
            }
            ContentBlock::Reasoning { .. } => {
                return Err(unsupported(
                    "DashScope reasoning replay requires reasoning_content support",
                ));
            }
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => tools.push(GenerationToolCallParam {
                id: id.clone(),
                kind: GenerationToolKind::Function,
                function: GenerationFunctionCallParam {
                    name: name.clone(),
                    arguments: arguments.to_string(),
                },
            }),
            ContentBlock::ToolResult { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::Document { .. } => {
                return Err(invalid("DashScope assistant message contains user content"));
            }
        }
    }
    output.push(GenerationMessageParam::Assistant {
        content: text,
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
        tool_calls: tools,
    });
    Ok(())
}

fn result_text(content: &[ToolResultContent]) -> Result<String, ProviderError> {
    let mut output = String::new();
    for item in content {
        match item {
            ToolResultContent::Text { text } => output.push_str(text),
            ToolResultContent::Image { .. } | ToolResultContent::Document { .. } => {
                return Err(unsupported(
                    "DashScope Generation tool results must be textual",
                ));
            }
        }
    }
    Ok(output)
}

pub(crate) fn response(
    provider: &str,
    model: &str,
    response: GenerationCompletion,
) -> Result<ModelResponse, ProviderError> {
    let mut content = Vec::new();
    if !response.reasoning_content.is_empty() {
        content.push(ContentBlock::Reasoning {
            text: response.reasoning_content,
            opaque_state: None,
        });
    }
    if !response.content.is_empty() {
        content.push(ContentBlock::Text {
            text: response.content,
        });
    }
    for tool in response.tool_calls {
        content.push(ContentBlock::ToolCall {
            id: tool.id,
            name: tool.name,
            arguments: serde_json::from_str(&tool.arguments)
                .map_err(|_| protocol("DashScope tool arguments are invalid JSON"))?,
        });
    }
    let stop_reason = match response.finish_reason.as_str() {
        "tool_calls" => StopReason::ToolUse,
        "length" => StopReason::MaxOutputTokens,
        "stop" => StopReason::EndTurn,
        value => StopReason::Other(value.into()),
    };
    Ok(ModelResponse {
        id: response.request_id,
        model: ModelRef::new(provider, model),
        content,
        stop_reason,
        usage: usage(response.usage),
    })
}

fn usage(value: GenerationUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: value.input_tokens,
        output_tokens: value.output_tokens,
        cache_read_tokens: value
            .prompt_tokens_details
            .and_then(|details| details.cached_tokens),
        details: TokenUsageDetails {
            reported_total_tokens: value.total_tokens,
            reasoning_tokens: value
                .output_tokens_details
                .and_then(|details| details.reasoning_tokens),
            ..TokenUsageDetails::default()
        },
        ..TokenUsage::default()
    }
}

pub(crate) fn error(error: DashScopeError, phase: ProviderErrorPhase) -> ProviderError {
    let kind = match &error {
        DashScopeError::Http(source) if source.is_timeout() => ProviderErrorKind::Timeout,
        DashScopeError::Http(_) => ProviderErrorKind::Transport,
        DashScopeError::Api { status: 401, .. } => ProviderErrorKind::Authentication,
        DashScopeError::Api { status: 402, .. } => ProviderErrorKind::QuotaExceeded,
        DashScopeError::Api { status: 403, .. } => ProviderErrorKind::PermissionDenied,
        DashScopeError::Api { status: 404, .. } => ProviderErrorKind::ModelNotFound,
        DashScopeError::Api { status: 429, .. } => ProviderErrorKind::RateLimited,
        DashScopeError::Api { status, .. } if *status >= 500 => ProviderErrorKind::Unavailable,
        DashScopeError::Api { .. } => ProviderErrorKind::InvalidRequest,
        DashScopeError::Json(_) | DashScopeError::Sse(_) | DashScopeError::Protocol(_) => {
            ProviderErrorKind::Protocol
        }
    };
    let mut output = ProviderError::new(kind, phase, "DashScope provider request failed");
    output.status = error.status();
    output.request_id = error.request_id().map(str::to_owned);
    output
}

fn invalid(message: &'static str) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidRequest,
        ProviderErrorPhase::Open,
        message,
    )
}

fn unsupported(message: &'static str) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Unsupported,
        ProviderErrorPhase::Open,
        message,
    )
}

fn protocol(message: &'static str) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Protocol,
        ProviderErrorPhase::Stream,
        message,
    )
}
