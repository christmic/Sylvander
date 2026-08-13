//! Conversion between neutral calls and Chat Completions wire types.

use sylvander_llm_core::{
    ChatMessage, ChatRole, ContentBlock, ModelRef, ModelRequest, ModelResponse, ProviderError,
    StopReason, TokenUsage, TokenUsageDetails,
};

use crate::ProviderFeatures;
use crate::api::chat::{
    ChatCompletion, ChatCompletionUsage, ChatFunction, ChatFunctionDefinition, ChatFunctionTool,
    ChatImageUrl, ChatInputAudio, ChatJsonSchema, ChatMessageParam, ChatResponseFormat,
    ChatStreamOptions, ChatToolCallParam, ChatUserContentPart, CreateChatCompletionRequest,
};
use crate::convert::common::{effort, media_url, result_text};
use crate::convert::invalid;
use crate::convert::protocol;

pub(crate) fn chat_request(
    features: &ProviderFeatures,
    input: &ModelRequest,
) -> Result<CreateChatCompletionRequest, ProviderError> {
    let mut messages = input
        .system
        .iter()
        .map(|value| ChatMessageParam::System {
            content: value.text.clone(),
        })
        .collect::<Vec<_>>();
    for message in &input.messages {
        append_message(message, features, &mut messages)?;
    }
    let tools = input
        .tools
        .iter()
        .map(|tool| ChatFunctionTool {
            kind: "function".into(),
            function: ChatFunctionDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
                strict: false,
            },
        })
        .collect();
    let max_completion = features.contains("max_completion_tokens");
    Ok(CreateChatCompletionRequest {
        model: input.model.model.clone(),
        messages,
        stream: true,
        stream_options: ChatStreamOptions {
            include_usage: true,
        },
        max_tokens: (!max_completion).then_some(input.max_output_tokens),
        max_completion_tokens: max_completion.then_some(input.max_output_tokens),
        tools,
        reasoning_effort: input
            .reasoning
            .and_then(|reasoning| reasoning.effort)
            .map(effort)
            .map(str::to_owned),
        response_format: input
            .output_schema
            .as_ref()
            .map(|schema| ChatResponseFormat {
                kind: "json_schema".into(),
                json_schema: ChatJsonSchema {
                    name: "response".into(),
                    schema: schema.clone(),
                    strict: true,
                },
            }),
        enable_thinking: (input.reasoning.is_some() && features.contains("enable_thinking"))
            .then_some(true),
    })
}

fn append_message(
    message: &ChatMessage,
    features: &ProviderFeatures,
    output: &mut Vec<ChatMessageParam>,
) -> Result<(), ProviderError> {
    let tool_results = message
        .content
        .iter()
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .count();
    if tool_results > 0 {
        if message.role != ChatRole::User || tool_results != message.content.len() {
            return Err(invalid(
                "Chat Completions requires tool results in dedicated user messages",
            ));
        }
        for block in &message.content {
            let ContentBlock::ToolResult {
                call_id, content, ..
            } = block
            else {
                unreachable!("validated tool-result-only message")
            };
            output.push(ChatMessageParam::Tool {
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
    output: &mut Vec<ChatMessageParam>,
) -> Result<(), ProviderError> {
    let mut content = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => {
                content.push(ChatUserContentPart::Text { text: text.clone() });
            }
            ContentBlock::Image { image } => {
                content.push(ChatUserContentPart::ImageUrl {
                    image_url: ChatImageUrl {
                        url: media_url(&image.source),
                    },
                });
            }
            ContentBlock::Audio { audio } => {
                content.push(ChatUserContentPart::InputAudio {
                    input_audio: ChatInputAudio {
                        data: audio.data.clone(),
                        format: audio.format.as_str().into(),
                    },
                });
            }
            ContentBlock::Document { .. } => {
                return Err(invalid("Chat Completions document input is not supported"));
            }
            ContentBlock::Reasoning { .. } | ContentBlock::ToolCall { .. } => {
                return Err(invalid("Chat user message contains assistant content"));
            }
            ContentBlock::ToolResult { .. } => unreachable!("handled above"),
        }
    }
    output.push(ChatMessageParam::User { content });
    Ok(())
}

fn append_assistant(
    message: &ChatMessage,
    features: &ProviderFeatures,
    output: &mut Vec<ChatMessageParam>,
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
                return Err(invalid(
                    "Chat reasoning replay requires reasoning_content support",
                ));
            }
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => tools.push(ChatToolCallParam {
                id: id.clone(),
                kind: "function".into(),
                function: ChatFunction {
                    name: name.clone(),
                    arguments: arguments.to_string(),
                },
            }),
            ContentBlock::ToolResult { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::Audio { .. }
            | ContentBlock::Document { .. } => {
                return Err(invalid("Chat assistant message contains user content"));
            }
        }
    }
    output.push(ChatMessageParam::Assistant {
        content: text,
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
        tool_calls: tools,
    });
    Ok(())
}

pub(crate) fn chat_response(
    provider: &str,
    response: ChatCompletion,
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
    if !response.refusal.is_empty() {
        content.push(ContentBlock::Text {
            text: response.refusal,
        });
    }
    for tool in response.tool_calls {
        content.push(ContentBlock::ToolCall {
            id: tool.id,
            name: tool.name,
            arguments: serde_json::from_str(&tool.arguments)
                .map_err(|_| protocol("Chat tool arguments are invalid JSON"))?,
        });
    }
    let stop_reason = match response.finish_reason.as_str() {
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "length" => StopReason::MaxOutputTokens,
        "content_filter" => StopReason::Refusal,
        "stop" => StopReason::EndTurn,
        value => StopReason::Other(value.into()),
    };
    let usage = response
        .usage
        .ok_or_else(|| protocol("Chat stream completed without requested usage"))?;
    Ok(ModelResponse {
        id: response.id,
        model: ModelRef::new(provider, response.model),
        content,
        stop_reason,
        usage: chat_usage(usage),
    })
}

fn chat_usage(value: ChatCompletionUsage) -> TokenUsage {
    let prompt = value.prompt_tokens_details;
    let completion = value.completion_tokens_details;
    TokenUsage {
        input_tokens: value.prompt_tokens,
        output_tokens: value.completion_tokens,
        cache_write_tokens: prompt.and_then(|details| details.cache_write_tokens),
        cache_read_tokens: prompt.and_then(|details| details.cached_tokens),
        details: TokenUsageDetails {
            reported_total_tokens: Some(value.total_tokens),
            reasoning_tokens: completion.and_then(|details| details.reasoning_tokens),
            audio_input_tokens: prompt.and_then(|details| details.audio_tokens),
            audio_output_tokens: completion.and_then(|details| details.audio_tokens),
            accepted_prediction_tokens: completion
                .and_then(|details| details.accepted_prediction_tokens),
            rejected_prediction_tokens: completion
                .and_then(|details| details.rejected_prediction_tokens),
            ..TokenUsageDetails::default()
        },
    }
}
