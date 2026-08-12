//! Conversion between neutral calls and `OpenAI` Responses wire types.

use sylvander_llm_core::{
    ChatMessage, ChatRole, ContentBlock, ModelRef, ModelRequest, ModelResponse,
    OpaqueProviderState, ProviderError, StopReason, TokenUsage, TokenUsageDetails,
};

use crate::ProviderFeatures;
use crate::api::responses::{
    CreateResponseRequest, MessageContent, Response, ResponseFunctionTool, ResponseInputContent,
    ResponseInputItem, ResponseJsonSchemaFormat, ResponseOutputItem, ResponseReasoning,
    ResponseTextConfig, ResponseUsage,
};
use crate::convert::common::{effort, media_url, result_text};
use crate::convert::invalid;
use crate::convert::protocol;

pub(crate) fn responses_request(
    features: &ProviderFeatures,
    input: &ModelRequest,
) -> Result<CreateResponseRequest, ProviderError> {
    let mut items = Vec::new();
    for message in &input.messages {
        append_message(message, &input.model.provider, &mut items)?;
    }
    let tools = input
        .tools
        .iter()
        .map(|tool| ResponseFunctionTool {
            kind: "function".into(),
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
            strict: false,
        })
        .collect();
    let reasoning = input.reasoning.and_then(|reasoning| {
        reasoning.effort.map(|value| ResponseReasoning {
            effort: effort(value).into(),
        })
    });
    let text = input
        .output_schema
        .as_ref()
        .map(|schema| ResponseTextConfig {
            format: ResponseJsonSchemaFormat {
                kind: "json_schema".into(),
                name: "response".into(),
                schema: schema.clone(),
                strict: true,
            },
        });
    let preserve_reasoning = input.reasoning.is_some()
        || input.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Reasoning { .. }))
        });
    Ok(CreateResponseRequest {
        model: input.model.model.clone(),
        input: items,
        instructions: (!input.system.is_empty()).then(|| {
            input
                .system
                .iter()
                .map(|value| value.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        }),
        max_output_tokens: input.max_output_tokens,
        stream: true,
        store: false,
        include: preserve_reasoning
            .then(|| "reasoning.encrypted_content".into())
            .into_iter()
            .collect(),
        tools,
        reasoning,
        text,
        enable_thinking: (input.reasoning.is_some() && features.contains("enable_thinking"))
            .then_some(true),
    })
}

fn append_message(
    message: &ChatMessage,
    provider: &str,
    output: &mut Vec<ResponseInputItem>,
) -> Result<(), ProviderError> {
    match message.role {
        ChatRole::User => append_user(message, output),
        ChatRole::Assistant => append_assistant(message, provider, output),
    }
}

fn append_user(
    message: &ChatMessage,
    output: &mut Vec<ResponseInputItem>,
) -> Result<(), ProviderError> {
    let has_results = message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolResult { .. }));
    if has_results
        && message
            .content
            .iter()
            .any(|block| !matches!(block, ContentBlock::ToolResult { .. }))
    {
        return Err(invalid(
            "Responses cannot preserve mixed tool results and user content",
        ));
    }
    if has_results {
        for block in &message.content {
            let ContentBlock::ToolResult {
                call_id, content, ..
            } = block
            else {
                unreachable!("validated tool-result-only message")
            };
            output.push(ResponseInputItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output: result_text(content)?,
            });
        }
        return Ok(());
    }
    let mut content = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => {
                content.push(ResponseInputContent::InputText { text: text.clone() });
            }
            ContentBlock::Image { image } => content.push(ResponseInputContent::InputImage {
                image_url: media_url(&image.source),
            }),
            ContentBlock::Document { document } => {
                let filename = document.title.as_deref().unwrap_or("document");
                let part = match &document.source {
                    sylvander_llm_core::MediaSource::Url { url } => {
                        ResponseInputContent::InputFile {
                            file_data: None,
                            file_url: Some(url.clone()),
                            filename: filename.into(),
                        }
                    }
                    sylvander_llm_core::MediaSource::Base64 { media_type, data } => {
                        ResponseInputContent::InputFile {
                            file_data: Some(format!("data:{media_type};base64,{data}")),
                            file_url: None,
                            filename: filename.into(),
                        }
                    }
                };
                content.push(part);
            }
            ContentBlock::Reasoning { .. } | ContentBlock::ToolCall { .. } => {
                return Err(invalid("Responses user message contains assistant content"));
            }
            ContentBlock::ToolResult { .. } => unreachable!("handled above"),
        }
    }
    output.push(ResponseInputItem::Message {
        role: "user".into(),
        content,
        status: None,
    });
    Ok(())
}

fn append_assistant(
    message: &ChatMessage,
    provider: &str,
    output: &mut Vec<ResponseInputItem>,
) -> Result<(), ProviderError> {
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => output.push(ResponseInputItem::Message {
                role: "assistant".into(),
                status: Some("completed".into()),
                content: vec![ResponseInputContent::OutputText {
                    text: text.clone(),
                    annotations: Vec::new(),
                }],
            }),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => output.push(ResponseInputItem::FunctionCall {
                call_id: id.clone(),
                name: name.clone(),
                arguments: arguments.to_string(),
            }),
            ContentBlock::Reasoning {
                opaque_state: Some(state),
                ..
            } if state.provider == provider => {
                let item: ResponseOutputItem = serde_json::from_value(state.data.clone())
                    .map_err(|_| invalid("Responses reasoning replay state is invalid"))?;
                let ResponseOutputItem::Reasoning(reasoning) = item else {
                    return Err(invalid("Responses replay state is not reasoning"));
                };
                output.push(ResponseInputItem::Reasoning(reasoning));
            }
            ContentBlock::Reasoning { .. } => {
                return Err(invalid("Responses reasoning replay state is missing"));
            }
            ContentBlock::ToolResult { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::Document { .. } => {
                return Err(invalid("Responses assistant message contains user content"));
            }
        }
    }
    Ok(())
}

pub(crate) fn responses_response(
    provider: &str,
    response: Response,
) -> Result<ModelResponse, ProviderError> {
    let mut content = Vec::new();
    let mut refused = false;
    for item in response.output {
        match item {
            ResponseOutputItem::Message(message) => {
                for part in message.content {
                    match part {
                        MessageContent::OutputText { text, .. } => {
                            content.push(ContentBlock::Text { text });
                        }
                        MessageContent::Refusal { refusal } => {
                            refused = true;
                            content.push(ContentBlock::Text { text: refusal });
                        }
                        MessageContent::Unsupported => {
                            return Err(protocol("Responses returned unsupported message content"));
                        }
                    }
                }
            }
            ResponseOutputItem::FunctionCall(tool) => {
                content.push(ContentBlock::ToolCall {
                    id: tool.call_id,
                    name: tool.name,
                    arguments: serde_json::from_str(&tool.arguments)
                        .map_err(|_| protocol("Responses tool arguments are invalid JSON"))?,
                });
            }
            ResponseOutputItem::Reasoning(reasoning) => {
                let text = reasoning
                    .summary
                    .iter()
                    .map(|part| part.text.as_str())
                    .collect::<String>();
                let data = serde_json::to_value(ResponseOutputItem::Reasoning(reasoning))
                    .map_err(|_| protocol("Responses reasoning state cannot be persisted"))?;
                content.push(ContentBlock::Reasoning {
                    text,
                    opaque_state: Some(OpaqueProviderState {
                        provider: provider.into(),
                        data,
                    }),
                });
            }
            ResponseOutputItem::Unsupported => {
                return Err(protocol("Responses returned an unsupported output item"));
            }
        }
    }
    let has_tool = content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolCall { .. }));
    let incomplete = response
        .incomplete_details
        .as_ref()
        .and_then(|details| details.reason.as_deref());
    let stop_reason = if has_tool {
        StopReason::ToolUse
    } else if incomplete == Some("max_output_tokens") {
        StopReason::MaxOutputTokens
    } else if refused || incomplete == Some("content_filter") {
        StopReason::Refusal
    } else if response.status == "completed" {
        StopReason::EndTurn
    } else {
        StopReason::Other(response.status.clone())
    };
    Ok(ModelResponse {
        id: response.id,
        model: ModelRef::new(provider, response.model),
        content,
        stop_reason,
        usage: response.usage.map(usage).unwrap_or_default(),
    })
}

fn usage(value: ResponseUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: value.input_tokens,
        output_tokens: value.output_tokens,
        cache_write_tokens: Some(value.input_tokens_details.cache_write_tokens),
        cache_read_tokens: Some(value.input_tokens_details.cached_tokens),
        details: TokenUsageDetails {
            reported_total_tokens: Some(value.total_tokens),
            reasoning_tokens: Some(value.output_tokens_details.reasoning_tokens),
            ..TokenUsageDetails::default()
        },
    }
}
