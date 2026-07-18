//! Private conversion boundary between neutral model types and Anthropic wire types.

#![allow(
    dead_code,
    reason = "conversion entry points are connected by the provider adapter in C2"
)]

use serde_json::{Value, json};
use sylvander_llm_core as core;

use crate::api::error::AnthropicError;
use crate::api::request::CreateMessageRequest;
use crate::api::types as wire;

pub(crate) fn request(
    input: &core::ModelRequest,
) -> Result<CreateMessageRequest, core::ProviderError> {
    let messages = input
        .messages
        .iter()
        .map(message)
        .collect::<Result<Vec<_>, _>>()?;
    let system = (!input.system.is_empty()).then(|| {
        wire::SystemPrompt::Blocks(
            input
                .system
                .iter()
                .map(|instruction| {
                    let block = wire::SystemTextBlock::new(instruction.text.clone());
                    wire::SystemBlock::Text(cache(block, instruction.cache_hint))
                })
                .collect(),
        )
    });
    let tools = input
        .tools
        .iter()
        .map(|tool| {
            let definition = wire::Tool::new(
                tool.name.clone(),
                tool.description.clone(),
                wire::InputSchema::from_json_value(tool.input_schema.clone()),
            );
            cache(definition, tool.cache_hint)
        })
        .collect();
    let output_config = input
        .output_schema
        .clone()
        .map(|schema| wire::OutputConfig {
            effort: None,
            format: Some(wire::JsonOutputFormat::new(schema)),
        });
    Ok(CreateMessageRequest {
        model: input.model.model.clone(),
        max_tokens: input.max_output_tokens,
        messages,
        system,
        tools,
        tool_choice: None,
        thinking: input
            .reasoning
            .map(|value| wire::ThinkingConfig::new(value.budget_tokens)),
        output_config,
        temperature: None,
        top_p: None,
        top_k: None,
        stop_sequences: Vec::new(),
    })
}

fn cache<T: Cacheable>(value: T, hint: Option<core::CacheHint>) -> T {
    match hint {
        Some(core::CacheHint::Ephemeral) => value.ephemeral(),
        None => value,
    }
}

trait Cacheable {
    fn ephemeral(self) -> Self;
}

impl Cacheable for wire::SystemTextBlock {
    fn ephemeral(self) -> Self {
        self.with_cache_control(wire::CacheControl::ephemeral())
    }
}

impl Cacheable for wire::Tool {
    fn ephemeral(self) -> Self {
        self.with_cache_control(wire::CacheControl::ephemeral())
    }
}

fn message(input: &core::ChatMessage) -> Result<wire::MessageParam, core::ProviderError> {
    match input.role {
        core::ChatRole::User => Ok(wire::MessageParam::user_blocks(
            input
                .content
                .iter()
                .map(user_block)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        core::ChatRole::Assistant => Ok(wire::MessageParam::assistant_blocks(
            input
                .content
                .iter()
                .map(assistant_block)
                .collect::<Result<Vec<_>, _>>()?,
        )),
    }
}

fn user_block(input: &core::ContentBlock) -> Result<wire::UserContentBlock, core::ProviderError> {
    match input {
        core::ContentBlock::Text { text } => Ok(wire::UserContentBlock::text(text.clone())),
        core::ContentBlock::ToolResult {
            call_id,
            content,
            is_error,
        } => {
            let blocks = content
                .iter()
                .map(tool_result)
                .collect::<Result<Vec<_>, _>>()?;
            let mut result = wire::ToolResultBlock::with_blocks(call_id.clone(), blocks);
            result.is_error = *is_error;
            Ok(wire::UserContentBlock::ToolResult(result))
        }
        core::ContentBlock::Image { image } => {
            Ok(wire::UserContentBlock::Image(image_block(image)?))
        }
        core::ContentBlock::Document { document } => Ok(wire::UserContentBlock::Other(json!({
            "type": "document",
            "source": media_json(&document.source),
            "title": document.title,
        }))),
        _ => Err(invalid(
            "user messages cannot contain reasoning or tool calls",
        )),
    }
}

fn assistant_block(input: &core::ContentBlock) -> Result<wire::ContentBlock, core::ProviderError> {
    match input {
        core::ContentBlock::Text { text } => {
            Ok(wire::ContentBlock::Text(wire::TextBlock::new(text)))
        }
        core::ContentBlock::ToolCall {
            id,
            name,
            arguments,
        } => Ok(wire::ContentBlock::ToolUse(wire::ToolUseBlock::new(
            id,
            name,
            arguments.clone(),
        ))),
        core::ContentBlock::Reasoning { text, opaque_state } => {
            let state = opaque_state
                .as_ref()
                .filter(|state| state.provider == "anthropic")
                .ok_or_else(|| invalid("Anthropic reasoning requires Anthropic opaque state"))?;
            let signature = state
                .data
                .get("signature")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Anthropic reasoning state has no signature"))?;
            Ok(wire::ContentBlock::Thinking(wire::ThinkingBlock::new(
                text, signature,
            )))
        }
        _ => Err(invalid(
            "assistant messages contain unsupported input content",
        )),
    }
}

fn tool_result(
    input: &core::ToolResultContent,
) -> Result<wire::RichToolResultBlock, core::ProviderError> {
    match input {
        core::ToolResultContent::Text { text } => Ok(wire::RichToolResultBlock::Text {
            text: text.clone(),
            cache_control: None,
        }),
        core::ToolResultContent::Image { image } => {
            Ok(wire::RichToolResultBlock::Image(image_block(image)?))
        }
        core::ToolResultContent::Document { document } => {
            Ok(wire::RichToolResultBlock::Other(json!({
                "type": "document",
                "source": media_json(&document.source),
                "title": document.title,
            })))
        }
    }
}

fn image_block(input: &core::ImageContent) -> Result<wire::ImageBlock, core::ProviderError> {
    let core::MediaSource::Base64 { media_type, data } = &input.source else {
        return Err(unsupported(
            "Anthropic adapter requires inline base64 images",
        ));
    };
    let media_type = match media_type.as_str() {
        "image/jpeg" => wire::ImageMediaType::Jpeg,
        "image/png" => wire::ImageMediaType::Png,
        "image/gif" => wire::ImageMediaType::Gif,
        "image/webp" => wire::ImageMediaType::Webp,
        _ => {
            return Err(unsupported(
                "Anthropic adapter does not support this image media type",
            ));
        }
    };
    Ok(wire::ImageBlock {
        kind: wire::image::ImageBlockKind::Image,
        source: wire::ImageSource::Base64(wire::Base64ImageSource::new(media_type, data)),
        cache_control: None,
    })
}

fn media_json(source: &core::MediaSource) -> Value {
    match source {
        core::MediaSource::Base64 { media_type, data } => {
            json!({"type": "base64", "media_type": media_type, "data": data})
        }
        core::MediaSource::Url { url } => json!({"type": "url", "url": url}),
    }
}

pub(crate) fn response(provider: &str, input: wire::Message) -> core::ModelResponse {
    let content = input.content.into_iter().map(response_block).collect();
    let stop_reason = match input.stop_reason.unwrap_or(wire::StopReason::Other) {
        wire::StopReason::EndTurn => core::StopReason::EndTurn,
        wire::StopReason::ToolUse => core::StopReason::ToolUse,
        wire::StopReason::MaxTokens => core::StopReason::MaxOutputTokens,
        wire::StopReason::StopSequence => {
            core::StopReason::StopSequence(input.stop_sequence.unwrap_or_default())
        }
        wire::StopReason::Refusal => core::StopReason::Refusal,
        wire::StopReason::PauseTurn => core::StopReason::Paused,
        wire::StopReason::Other => core::StopReason::Other("anthropic_other".into()),
    };
    core::ModelResponse {
        id: input.id,
        model: core::ModelRef::new(provider, input.model),
        content,
        stop_reason,
        usage: usage(&input.usage),
    }
}

fn response_block(input: wire::ContentBlock) -> core::ContentBlock {
    match input {
        wire::ContentBlock::Text(value) => core::ContentBlock::Text { text: value.text },
        wire::ContentBlock::ToolUse(value) => core::ContentBlock::ToolCall {
            id: value.id,
            name: value.name,
            arguments: value.input,
        },
        wire::ContentBlock::Thinking(value) => core::ContentBlock::Reasoning {
            text: value.thinking,
            opaque_state: Some(core::OpaqueProviderState {
                provider: "anthropic".into(),
                data: json!({"signature": value.signature}),
            }),
        },
    }
}

pub(crate) fn usage(input: &wire::Usage) -> core::TokenUsage {
    core::TokenUsage {
        input_tokens: u64::from(input.input_tokens),
        output_tokens: u64::from(input.output_tokens),
        cache_write_tokens: input.cache_creation_input_tokens.map(u64::from),
        cache_read_tokens: input.cache_read_input_tokens.map(u64::from),
    }
}

pub(crate) fn error(input: AnthropicError, phase: core::ProviderErrorPhase) -> core::ProviderError {
    let (kind, status, request_id) = match &input {
        AnthropicError::Http(value) if value.is_timeout() => {
            (core::ProviderErrorKind::Timeout, None, None)
        }
        AnthropicError::Http(_) => (core::ProviderErrorKind::Transport, None, None),
        AnthropicError::Api {
            status, request_id, ..
        } => (
            match *status {
                401 => core::ProviderErrorKind::Authentication,
                403 => core::ProviderErrorKind::PermissionDenied,
                404 => core::ProviderErrorKind::ModelNotFound,
                429 => core::ProviderErrorKind::RateLimited,
                500..=u16::MAX => core::ProviderErrorKind::Unavailable,
                _ => core::ProviderErrorKind::InvalidRequest,
            },
            Some(*status),
            request_id.clone(),
        ),
        AnthropicError::Validation(_) | AnthropicError::Json(_) => {
            (core::ProviderErrorKind::InvalidRequest, None, None)
        }
        AnthropicError::SseParse { .. }
        | AnthropicError::UnknownBlockType(_)
        | AnthropicError::UnknownStreamEventType(_) => {
            (core::ProviderErrorKind::Protocol, None, None)
        }
    };
    let message = match kind {
        core::ProviderErrorKind::Transport => "model provider transport failed",
        core::ProviderErrorKind::Timeout => "model provider request timed out",
        core::ProviderErrorKind::RateLimited => "model provider rate limit reached",
        core::ProviderErrorKind::Authentication => "model provider authentication failed",
        core::ProviderErrorKind::PermissionDenied => "model provider denied the request",
        core::ProviderErrorKind::ModelNotFound => "requested model is unavailable",
        core::ProviderErrorKind::InvalidRequest => "model provider rejected the request",
        core::ProviderErrorKind::Unavailable => "model provider is unavailable",
        core::ProviderErrorKind::Protocol => "model provider returned an invalid response",
        _ => "model provider request failed",
    };
    let mut result = core::ProviderError::new(kind, phase, message);
    result.status = status;
    result.request_id = request_id;
    result
}

fn invalid(message: &str) -> core::ProviderError {
    core::ProviderError::new(
        core::ProviderErrorKind::InvalidRequest,
        core::ProviderErrorPhase::Open,
        message,
    )
}

fn unsupported(message: &str) -> core::ProviderError {
    core::ProviderError::new(
        core::ProviderErrorKind::Unsupported,
        core::ProviderErrorPhase::Open,
        message,
    )
}

#[cfg(test)]
#[path = "../tests/unit/convert.rs"]
mod tests;
