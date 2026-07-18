use serde_json::{Map, Value, json};
use sylvander_llm_anthropic::api::types as anthropic_wire;
use sylvander_llm_core as core;

use super::{ANTHROPIC, ProviderAdapterError, require_anthropic};

pub(crate) fn message_to_core(
    message: &anthropic_wire::MessageParam,
) -> Result<core::ChatMessage, ProviderAdapterError> {
    let role = match message.role {
        anthropic_wire::MessageRole::User => core::ChatRole::User,
        anthropic_wire::MessageRole::Assistant => core::ChatRole::Assistant,
    };
    let content = match &message.content {
        anthropic_wire::UserContent::String(text) => {
            vec![core::ContentBlock::Text { text: text.clone() }]
        }
        anthropic_wire::UserContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| block_to_core(message.role, block))
            .collect::<Result<_, _>>()?,
    };
    Ok(core::ChatMessage { role, content })
}

#[cfg(test)]
pub(crate) fn message_from_core(
    message: &core::ChatMessage,
) -> Result<anthropic_wire::MessageParam, ProviderAdapterError> {
    let (role, content) = match message.role {
        core::ChatRole::User => (
            anthropic_wire::MessageRole::User,
            message
                .content
                .iter()
                .map(user_block_from_core)
                .collect::<Result<_, _>>()?,
        ),
        core::ChatRole::Assistant => (
            anthropic_wire::MessageRole::Assistant,
            message
                .content
                .iter()
                .map(assistant_block_from_core)
                .collect::<Result<_, _>>()?,
        ),
    };
    Ok(anthropic_wire::MessageParam {
        role,
        content: anthropic_wire::UserContent::Blocks(content),
    })
}

pub(crate) fn response_from_core(
    response: core::ModelResponse,
) -> Result<anthropic_wire::Message, ProviderAdapterError> {
    let content = response
        .content
        .into_iter()
        .map(response_block_from_core)
        .collect::<Result<_, _>>()?;
    let (stop_reason, stop_sequence) = match response.stop_reason {
        core::StopReason::EndTurn => (anthropic_wire::StopReason::EndTurn, None),
        core::StopReason::ToolUse => (anthropic_wire::StopReason::ToolUse, None),
        core::StopReason::MaxOutputTokens => (anthropic_wire::StopReason::MaxTokens, None),
        core::StopReason::StopSequence(value) => {
            (anthropic_wire::StopReason::StopSequence, Some(value))
        }
        core::StopReason::Refusal => (anthropic_wire::StopReason::Refusal, None),
        core::StopReason::Paused => (anthropic_wire::StopReason::PauseTurn, None),
        core::StopReason::Other(_) => {
            return Err(ProviderAdapterError::Unsupported(
                "provider-specific stop reason",
            ));
        }
    };
    Ok(anthropic_wire::Message {
        id: response.id,
        kind: anthropic_wire::MessageKind::Message,
        role: anthropic_wire::MessageRole::Assistant,
        content,
        model: response.model.model,
        stop_reason: Some(stop_reason),
        stop_sequence,
        usage: anthropic_wire::Usage {
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
    role: anthropic_wire::MessageRole,
    block: &anthropic_wire::UserContentBlock,
) -> Result<core::ContentBlock, ProviderAdapterError> {
    match block {
        anthropic_wire::UserContentBlock::Text(text) if text.cache_control.is_none() => {
            Ok(core::ContentBlock::Text {
                text: text.text.clone(),
            })
        }
        anthropic_wire::UserContentBlock::Image(image)
            if role == anthropic_wire::MessageRole::User =>
        {
            Ok(core::ContentBlock::Image {
                image: image_to_core(image)?,
            })
        }
        anthropic_wire::UserContentBlock::ToolResult(result)
            if role == anthropic_wire::MessageRole::User =>
        {
            result_to_core(result)
        }
        anthropic_wire::UserContentBlock::Other(value)
            if role == anthropic_wire::MessageRole::Assistant =>
        {
            assistant_json_to_core(value)
        }
        anthropic_wire::UserContentBlock::Text(_) => Err(ProviderAdapterError::Unsupported(
            "message text cache control",
        )),
        _ => Err(ProviderAdapterError::Unsupported(
            "content block for message role",
        )),
    }
}

#[cfg(test)]
fn user_block_from_core(
    block: &core::ContentBlock,
) -> Result<anthropic_wire::UserContentBlock, ProviderAdapterError> {
    match block {
        core::ContentBlock::Text { text } => Ok(anthropic_wire::UserContentBlock::text(text)),
        core::ContentBlock::Image { image } => Ok(anthropic_wire::UserContentBlock::Image(
            image_from_core(image)?,
        )),
        core::ContentBlock::ToolResult {
            call_id,
            content,
            is_error,
        } => Ok(anthropic_wire::UserContentBlock::ToolResult(
            result_from_core(call_id, content, *is_error)?,
        )),
        _ => Err(ProviderAdapterError::Unsupported("core user content block")),
    }
}

#[cfg(test)]
fn assistant_block_from_core(
    block: &core::ContentBlock,
) -> Result<anthropic_wire::UserContentBlock, ProviderAdapterError> {
    match response_block_from_core(block.clone())? {
        anthropic_wire::ContentBlock::Text(text) => {
            Ok(anthropic_wire::UserContentBlock::Text(text))
        }
        anthropic_wire::ContentBlock::Thinking(block) => {
            Ok(anthropic_wire::UserContentBlock::Other(json!({
                "type": "thinking", "thinking": block.thinking, "signature": block.signature
            })))
        }
        anthropic_wire::ContentBlock::ToolUse(block) => {
            Ok(anthropic_wire::UserContentBlock::Other(json!({
                "type": "tool_use", "id": block.id, "name": block.name, "input": block.input
            })))
        }
    }
}

fn assistant_json_to_core(value: &Value) -> Result<core::ContentBlock, ProviderAdapterError> {
    let object = value.as_object().ok_or(ProviderAdapterError::Unsupported(
        "assistant opaque content",
    ))?;
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
                .ok_or(ProviderAdapterError::Unsupported("tool call input"))?,
        }),
        _ => Err(ProviderAdapterError::Unsupported(
            "assistant opaque content",
        )),
    }
}

fn result_to_core(
    result: &anthropic_wire::ToolResultBlock,
) -> Result<core::ContentBlock, ProviderAdapterError> {
    if result.cache_control.is_some() {
        return Err(ProviderAdapterError::Unsupported(
            "tool result cache control",
        ));
    }
    let content = match &result.content {
        None => Vec::new(),
        Some(anthropic_wire::ToolResultContent::String(text)) => {
            vec![core::ToolResultContent::Text { text: text.clone() }]
        }
        Some(anthropic_wire::ToolResultContent::Blocks(blocks)) => blocks
            .iter()
            .map(|block| match block {
                anthropic_wire::RichToolResultBlock::Text {
                    text,
                    cache_control: None,
                } => Ok(core::ToolResultContent::Text { text: text.clone() }),
                anthropic_wire::RichToolResultBlock::Image(image) => {
                    Ok(core::ToolResultContent::Image {
                        image: image_to_core(image)?,
                    })
                }
                _ => Err(ProviderAdapterError::Unsupported("rich tool result block")),
            })
            .collect::<Result<_, _>>()?,
    };
    Ok(core::ContentBlock::ToolResult {
        call_id: result.tool_use_id.clone(),
        content,
        is_error: result.is_error,
    })
}

#[cfg(test)]
fn result_from_core(
    call_id: &str,
    content: &[core::ToolResultContent],
    is_error: bool,
) -> Result<anthropic_wire::ToolResultBlock, ProviderAdapterError> {
    let blocks = content
        .iter()
        .map(|block| match block {
            core::ToolResultContent::Text { text } => {
                Ok(anthropic_wire::RichToolResultBlock::Text {
                    text: text.clone(),
                    cache_control: None,
                })
            }
            core::ToolResultContent::Image { image } => Ok(
                anthropic_wire::RichToolResultBlock::Image(image_from_core(image)?),
            ),
            core::ToolResultContent::Document { .. } => {
                Err(ProviderAdapterError::Unsupported("document tool result"))
            }
        })
        .collect::<Result<_, _>>()?;
    Ok(anthropic_wire::ToolResultBlock {
        kind: anthropic_wire::tool_result::ToolResultBlockKind::ToolResult,
        tool_use_id: call_id.into(),
        content: Some(anthropic_wire::ToolResultContent::Blocks(blocks)),
        is_error,
        cache_control: None,
    })
}

fn image_to_core(
    image: &anthropic_wire::ImageBlock,
) -> Result<core::ImageContent, ProviderAdapterError> {
    if image.cache_control.is_some() {
        return Err(ProviderAdapterError::Unsupported("image cache control"));
    }
    let anthropic_wire::ImageSource::Base64(source) = &image.source;
    Ok(core::ImageContent {
        source: core::MediaSource::Base64 {
            media_type: mime(source.media_type).into(),
            data: source.data.clone(),
        },
        alt_text: None,
    })
}

#[cfg(test)]
fn image_from_core(
    image: &core::ImageContent,
) -> Result<anthropic_wire::ImageBlock, ProviderAdapterError> {
    if image.alt_text.is_some() {
        return Err(ProviderAdapterError::Unsupported("image alt text"));
    }
    let core::MediaSource::Base64 { media_type, data } = &image.source else {
        return Err(ProviderAdapterError::Unsupported("URL image"));
    };
    let media_type = match media_type.as_str() {
        "image/jpeg" => anthropic_wire::ImageMediaType::Jpeg,
        "image/png" => anthropic_wire::ImageMediaType::Png,
        "image/gif" => anthropic_wire::ImageMediaType::Gif,
        "image/webp" => anthropic_wire::ImageMediaType::Webp,
        _ => return Err(ProviderAdapterError::Unsupported("image media type")),
    };
    Ok(anthropic_wire::ImageBlock {
        kind: anthropic_wire::image::ImageBlockKind::Image,
        source: anthropic_wire::ImageSource::Base64(anthropic_wire::Base64ImageSource::new(
            media_type, data,
        )),
        cache_control: None,
    })
}

fn response_block_from_core(
    block: core::ContentBlock,
) -> Result<anthropic_wire::ContentBlock, ProviderAdapterError> {
    match block {
        core::ContentBlock::Text { text } => Ok(anthropic_wire::ContentBlock::Text(
            anthropic_wire::TextBlock::new(text),
        )),
        core::ContentBlock::ToolCall {
            id,
            name,
            arguments,
        } => Ok(anthropic_wire::ContentBlock::ToolUse(
            anthropic_wire::ToolUseBlock::new(id, name, arguments),
        )),
        core::ContentBlock::Reasoning { text, opaque_state } => {
            Ok(anthropic_wire::ContentBlock::Thinking(
                anthropic_wire::ThinkingBlock::new(text, signature(opaque_state.as_ref())?),
            ))
        }
        _ => Err(ProviderAdapterError::Unsupported(
            "core response content block",
        )),
    }
}

fn signature(state: Option<&core::OpaqueProviderState>) -> Result<&str, ProviderAdapterError> {
    let state = state.ok_or(ProviderAdapterError::Unsupported("unsigned reasoning"))?;
    require_anthropic(&state.provider)?;
    let object = state
        .data
        .as_object()
        .ok_or(ProviderAdapterError::Unsupported(
            "Anthropic reasoning state",
        ))?;
    if object.len() != 1 {
        return Err(ProviderAdapterError::Unsupported(
            "Anthropic reasoning state fields",
        ));
    }
    string(object, "signature")
}

fn string<'a>(
    object: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a str, ProviderAdapterError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or(ProviderAdapterError::Unsupported(key))
}

fn checked(field: &'static str, value: u64) -> Result<u32, ProviderAdapterError> {
    u32::try_from(value).map_err(|_| ProviderAdapterError::NumericOverflow { field, value })
}

fn optional(field: &'static str, value: Option<u64>) -> Result<Option<u32>, ProviderAdapterError> {
    value.map(|value| checked(field, value)).transpose()
}

fn mime(value: anthropic_wire::ImageMediaType) -> &'static str {
    match value {
        anthropic_wire::ImageMediaType::Jpeg => "image/jpeg",
        anthropic_wire::ImageMediaType::Png => "image/png",
        anthropic_wire::ImageMediaType::Gif => "image/gif",
        anthropic_wire::ImageMediaType::Webp => "image/webp",
    }
}

#[cfg(test)]
#[path = "../../tests/unit/provider_adapter_content.rs"]
mod tests;
