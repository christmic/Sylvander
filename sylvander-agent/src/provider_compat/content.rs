use serde_json::{Map, Value, json};
use sylvander_llm_anthropic::api::types as legacy;
use sylvander_llm_core as core;

use super::{ANTHROPIC, ProviderCompatError, require_anthropic};

pub fn message_to_core(
    message: &legacy::MessageParam,
) -> Result<core::ChatMessage, ProviderCompatError> {
    let role = match message.role {
        legacy::MessageRole::User => core::ChatRole::User,
        legacy::MessageRole::Assistant => core::ChatRole::Assistant,
    };
    let content = match &message.content {
        legacy::UserContent::String(text) => vec![core::ContentBlock::Text { text: text.clone() }],
        legacy::UserContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| block_to_core(message.role, block))
            .collect::<Result<_, _>>()?,
    };
    Ok(core::ChatMessage { role, content })
}

pub fn message_from_core(
    message: &core::ChatMessage,
) -> Result<legacy::MessageParam, ProviderCompatError> {
    let (role, content) = match message.role {
        core::ChatRole::User => (
            legacy::MessageRole::User,
            message
                .content
                .iter()
                .map(user_block_from_core)
                .collect::<Result<_, _>>()?,
        ),
        core::ChatRole::Assistant => (
            legacy::MessageRole::Assistant,
            message
                .content
                .iter()
                .map(assistant_block_from_core)
                .collect::<Result<_, _>>()?,
        ),
    };
    Ok(legacy::MessageParam {
        role,
        content: legacy::UserContent::Blocks(content),
    })
}

pub fn response_from_core(
    response: core::ModelResponse,
) -> Result<legacy::Message, ProviderCompatError> {
    let content = response
        .content
        .into_iter()
        .map(response_block_from_core)
        .collect::<Result<_, _>>()?;
    let (stop_reason, stop_sequence) = match response.stop_reason {
        core::StopReason::EndTurn => (legacy::StopReason::EndTurn, None),
        core::StopReason::ToolUse => (legacy::StopReason::ToolUse, None),
        core::StopReason::MaxOutputTokens => (legacy::StopReason::MaxTokens, None),
        core::StopReason::StopSequence(value) => (legacy::StopReason::StopSequence, Some(value)),
        core::StopReason::Refusal => (legacy::StopReason::Refusal, None),
        core::StopReason::Paused => (legacy::StopReason::PauseTurn, None),
        core::StopReason::Other(_) => {
            return Err(ProviderCompatError::Unsupported(
                "provider-specific stop reason",
            ));
        }
    };
    Ok(legacy::Message {
        id: response.id,
        kind: legacy::MessageKind::Message,
        role: legacy::MessageRole::Assistant,
        content,
        model: response.model.model,
        stop_reason: Some(stop_reason),
        stop_sequence,
        usage: legacy::Usage {
            input_tokens: checked("input_tokens", response.usage.input_tokens)?,
            output_tokens: checked("output_tokens", response.usage.output_tokens)?,
            cache_creation_input_tokens: optional(
                "cache_write_tokens",
                response.usage.cache_write_tokens,
            )?,
            cache_read_input_tokens: optional(
                "cache_read_tokens",
                response.usage.cache_read_tokens,
            )?,
        },
    })
}

fn block_to_core(
    role: legacy::MessageRole,
    block: &legacy::UserContentBlock,
) -> Result<core::ContentBlock, ProviderCompatError> {
    match block {
        legacy::UserContentBlock::Text(text) if text.cache_control.is_none() => {
            Ok(core::ContentBlock::Text {
                text: text.text.clone(),
            })
        }
        legacy::UserContentBlock::Image(image) if role == legacy::MessageRole::User => {
            Ok(core::ContentBlock::Image {
                image: image_to_core(image)?,
            })
        }
        legacy::UserContentBlock::ToolResult(result) if role == legacy::MessageRole::User => {
            result_to_core(result)
        }
        legacy::UserContentBlock::Other(value) if role == legacy::MessageRole::Assistant => {
            assistant_json_to_core(value)
        }
        legacy::UserContentBlock::Text(_) => Err(ProviderCompatError::Unsupported(
            "message text cache control",
        )),
        _ => Err(ProviderCompatError::Unsupported(
            "content block for message role",
        )),
    }
}

fn user_block_from_core(
    block: &core::ContentBlock,
) -> Result<legacy::UserContentBlock, ProviderCompatError> {
    match block {
        core::ContentBlock::Text { text } => Ok(legacy::UserContentBlock::text(text)),
        core::ContentBlock::Image { image } => {
            Ok(legacy::UserContentBlock::Image(image_from_core(image)?))
        }
        core::ContentBlock::ToolResult {
            call_id,
            content,
            is_error,
        } => Ok(legacy::UserContentBlock::ToolResult(result_from_core(
            call_id, content, *is_error,
        )?)),
        _ => Err(ProviderCompatError::Unsupported("core user content block")),
    }
}

fn assistant_block_from_core(
    block: &core::ContentBlock,
) -> Result<legacy::UserContentBlock, ProviderCompatError> {
    match response_block_from_core(block.clone())? {
        legacy::ContentBlock::Text(text) => Ok(legacy::UserContentBlock::Text(text)),
        legacy::ContentBlock::Thinking(block) => Ok(legacy::UserContentBlock::Other(json!({
            "type": "thinking", "thinking": block.thinking, "signature": block.signature
        }))),
        legacy::ContentBlock::ToolUse(block) => Ok(legacy::UserContentBlock::Other(json!({
            "type": "tool_use", "id": block.id, "name": block.name, "input": block.input
        }))),
    }
}

fn assistant_json_to_core(value: &Value) -> Result<core::ContentBlock, ProviderCompatError> {
    let object = value
        .as_object()
        .ok_or(ProviderCompatError::Unsupported("assistant opaque content"))?;
    match string(object, "type")? {
        "thinking" if object.len() == 3 => Ok(core::ContentBlock::Reasoning {
            text: string(object, "thinking")?.into(),
            opaque_state: Some(core::OpaqueProviderState {
                provider: ANTHROPIC.into(),
                data: json!({"signature": string(object, "signature")?}),
            }),
        }),
        "tool_use" if object.len() == 4 => Ok(core::ContentBlock::ToolCall {
            id: string(object, "id")?.into(),
            name: string(object, "name")?.into(),
            arguments: object
                .get("input")
                .cloned()
                .ok_or(ProviderCompatError::Unsupported("tool call input"))?,
        }),
        _ => Err(ProviderCompatError::Unsupported("assistant opaque content")),
    }
}

fn result_to_core(
    result: &legacy::ToolResultBlock,
) -> Result<core::ContentBlock, ProviderCompatError> {
    if result.cache_control.is_some() {
        return Err(ProviderCompatError::Unsupported(
            "tool result cache control",
        ));
    }
    let content = match &result.content {
        None => Vec::new(),
        Some(legacy::ToolResultContent::String(text)) => {
            vec![core::ToolResultContent::Text { text: text.clone() }]
        }
        Some(legacy::ToolResultContent::Blocks(blocks)) => blocks
            .iter()
            .map(|block| match block {
                legacy::RichToolResultBlock::Text {
                    text,
                    cache_control: None,
                } => Ok(core::ToolResultContent::Text { text: text.clone() }),
                legacy::RichToolResultBlock::Image(image) => Ok(core::ToolResultContent::Image {
                    image: image_to_core(image)?,
                }),
                _ => Err(ProviderCompatError::Unsupported("rich tool result block")),
            })
            .collect::<Result<_, _>>()?,
    };
    Ok(core::ContentBlock::ToolResult {
        call_id: result.tool_use_id.clone(),
        content,
        is_error: result.is_error,
    })
}

fn result_from_core(
    call_id: &str,
    content: &[core::ToolResultContent],
    is_error: bool,
) -> Result<legacy::ToolResultBlock, ProviderCompatError> {
    let blocks = content
        .iter()
        .map(|block| match block {
            core::ToolResultContent::Text { text } => Ok(legacy::RichToolResultBlock::Text {
                text: text.clone(),
                cache_control: None,
            }),
            core::ToolResultContent::Image { image } => {
                Ok(legacy::RichToolResultBlock::Image(image_from_core(image)?))
            }
            core::ToolResultContent::Document { .. } => {
                Err(ProviderCompatError::Unsupported("document tool result"))
            }
        })
        .collect::<Result<_, _>>()?;
    Ok(legacy::ToolResultBlock {
        kind: legacy::tool_result::ToolResultBlockKind::ToolResult,
        tool_use_id: call_id.into(),
        content: Some(legacy::ToolResultContent::Blocks(blocks)),
        is_error,
        cache_control: None,
    })
}

fn image_to_core(image: &legacy::ImageBlock) -> Result<core::ImageContent, ProviderCompatError> {
    if image.cache_control.is_some() {
        return Err(ProviderCompatError::Unsupported("image cache control"));
    }
    let legacy::ImageSource::Base64(source) = &image.source;
    Ok(core::ImageContent {
        source: core::MediaSource::Base64 {
            media_type: mime(source.media_type).into(),
            data: source.data.clone(),
        },
        alt_text: None,
    })
}

fn image_from_core(image: &core::ImageContent) -> Result<legacy::ImageBlock, ProviderCompatError> {
    if image.alt_text.is_some() {
        return Err(ProviderCompatError::Unsupported("image alt text"));
    }
    let core::MediaSource::Base64 { media_type, data } = &image.source else {
        return Err(ProviderCompatError::Unsupported("URL image"));
    };
    let media_type = match media_type.as_str() {
        "image/jpeg" => legacy::ImageMediaType::Jpeg,
        "image/png" => legacy::ImageMediaType::Png,
        "image/gif" => legacy::ImageMediaType::Gif,
        "image/webp" => legacy::ImageMediaType::Webp,
        _ => return Err(ProviderCompatError::Unsupported("image media type")),
    };
    Ok(legacy::ImageBlock {
        kind: legacy::image::ImageBlockKind::Image,
        source: legacy::ImageSource::Base64(legacy::Base64ImageSource::new(media_type, data)),
        cache_control: None,
    })
}

fn response_block_from_core(
    block: core::ContentBlock,
) -> Result<legacy::ContentBlock, ProviderCompatError> {
    match block {
        core::ContentBlock::Text { text } => {
            Ok(legacy::ContentBlock::Text(legacy::TextBlock::new(text)))
        }
        core::ContentBlock::ToolCall {
            id,
            name,
            arguments,
        } => Ok(legacy::ContentBlock::ToolUse(legacy::ToolUseBlock::new(
            id, name, arguments,
        ))),
        core::ContentBlock::Reasoning { text, opaque_state } => Ok(legacy::ContentBlock::Thinking(
            legacy::ThinkingBlock::new(text, signature(opaque_state.as_ref())?),
        )),
        _ => Err(ProviderCompatError::Unsupported(
            "core response content block",
        )),
    }
}

fn signature(state: Option<&core::OpaqueProviderState>) -> Result<&str, ProviderCompatError> {
    let state = state.ok_or(ProviderCompatError::Unsupported("unsigned reasoning"))?;
    require_anthropic(&state.provider)?;
    let object = state
        .data
        .as_object()
        .ok_or(ProviderCompatError::Unsupported(
            "Anthropic reasoning state",
        ))?;
    if object.len() != 1 {
        return Err(ProviderCompatError::Unsupported(
            "Anthropic reasoning state fields",
        ));
    }
    string(object, "signature")
}

fn string<'a>(
    object: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a str, ProviderCompatError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or(ProviderCompatError::Unsupported(key))
}

fn checked(field: &'static str, value: u64) -> Result<u32, ProviderCompatError> {
    u32::try_from(value).map_err(|_| ProviderCompatError::NumericOverflow { field, value })
}

fn optional(field: &'static str, value: Option<u64>) -> Result<Option<u32>, ProviderCompatError> {
    value.map(|value| checked(field, value)).transpose()
}

fn mime(value: legacy::ImageMediaType) -> &'static str {
    match value {
        legacy::ImageMediaType::Jpeg => "image/jpeg",
        legacy::ImageMediaType::Png => "image/png",
        legacy::ImageMediaType::Gif => "image/gif",
        legacy::ImageMediaType::Webp => "image/webp",
    }
}

#[cfg(test)]
#[path = "content_tests.rs"]
mod tests;
