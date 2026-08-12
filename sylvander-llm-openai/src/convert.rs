use serde_json::{Map, Value, json};
use sylvander_llm_core::{
    ChatMessage, ChatRole, ContentBlock, MediaSource, ModelRequest, ProviderError,
    ProviderErrorKind, ProviderErrorPhase, ReasoningEffort, ToolResultContent,
};

use crate::{OpenAiProtocol, ProviderFeatures, error};

pub(crate) fn request(
    protocol: OpenAiProtocol,
    features: &ProviderFeatures,
    input: &ModelRequest,
) -> Result<Value, ProviderError> {
    match protocol {
        OpenAiProtocol::Responses => responses(features, input),
        OpenAiProtocol::ChatCompletions => chat(features, input),
    }
}

fn responses(features: &ProviderFeatures, input: &ModelRequest) -> Result<Value, ProviderError> {
    let mut body = Map::new();
    body.insert("model".into(), json!(input.model.model));
    body.insert("stream".into(), Value::Bool(true));
    body.insert("max_output_tokens".into(), json!(input.max_output_tokens));
    body.insert("store".into(), Value::Bool(false));
    if !input.system.is_empty() {
        body.insert(
            "instructions".into(),
            json!(
                input
                    .system
                    .iter()
                    .map(|value| value.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            ),
        );
    }
    let mut items = Vec::new();
    for message in &input.messages {
        response_items(message, &input.model.provider, &mut items)?;
    }
    body.insert("input".into(), Value::Array(items));
    if !input.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(
                input
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema,
                            "strict": false,
                        })
                    })
                    .collect(),
            ),
        );
    }
    if let Some(reasoning) = input.reasoning {
        let mut value = Map::new();
        if let Some(effort) = reasoning.effort {
            value.insert("effort".into(), json!(effort_name(effort)));
        }
        if !value.is_empty() {
            body.insert("reasoning".into(), Value::Object(value));
        }
        if features.contains("enable_thinking") {
            body.insert("enable_thinking".into(), Value::Bool(true));
        }
    }
    if let Some(schema) = &input.output_schema {
        body.insert(
            "text".into(),
            json!({
                "format": {
                    "type": "json_schema",
                    "name": "response",
                    "schema": schema,
                    "strict": true,
                }
            }),
        );
    }
    Ok(Value::Object(body))
}

fn response_items(
    message: &ChatMessage,
    provider: &str,
    output: &mut Vec<Value>,
) -> Result<(), ProviderError> {
    match message.role {
        ChatRole::User => {
            let mut content = Vec::new();
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => {
                        content.push(json!({"type": "input_text", "text": text}));
                    }
                    ContentBlock::Image { image } => content.push(json!({
                        "type": "input_image",
                        "image_url": media_url(&image.source)?,
                    })),
                    ContentBlock::ToolResult {
                        call_id,
                        content: result,
                        ..
                    } => output.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": result_text(result)?,
                    })),
                    _ => return Err(invalid("unsupported Responses user content")),
                }
            }
            if !content.is_empty() {
                output.push(json!({"role": "user", "content": content}));
            }
        }
        ChatRole::Assistant => {
            let mut content = Vec::new();
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => {
                        content.push(json!({"type": "output_text", "text": text}));
                    }
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => output.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": arguments.to_string(),
                    })),
                    ContentBlock::Reasoning {
                        opaque_state: Some(state),
                        ..
                    } if state.provider == provider => output.push(state.data.clone()),
                    ContentBlock::Reasoning { .. } => {
                        return Err(invalid("Responses reasoning replay state is missing"));
                    }
                    _ => return Err(invalid("unsupported Responses assistant content")),
                }
            }
            if !content.is_empty() {
                output.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": content,
                }));
            }
        }
    }
    Ok(())
}

fn chat(features: &ProviderFeatures, input: &ModelRequest) -> Result<Value, ProviderError> {
    let mut messages = input
        .system
        .iter()
        .map(|value| json!({"role": "system", "content": value.text}))
        .collect::<Vec<_>>();
    for message in &input.messages {
        chat_messages(message, features, &mut messages)?;
    }
    let tools = input
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })
        })
        .collect::<Vec<_>>();
    let mut body = Map::new();
    body.insert("model".into(), json!(input.model.model));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), Value::Bool(true));
    body.insert(
        if features.contains("max_completion_tokens") {
            "max_completion_tokens"
        } else {
            "max_tokens"
        }
        .into(),
        json!(input.max_output_tokens),
    );
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }
    if input.reasoning.is_some() && features.contains("enable_thinking") {
        body.insert("enable_thinking".into(), Value::Bool(true));
    }
    if let Some(reasoning) = input.reasoning
        && let Some(effort) = reasoning.effort
    {
        body.insert("reasoning_effort".into(), json!(effort_name(effort)));
    }
    if let Some(schema) = &input.output_schema {
        body.insert(
            "response_format".into(),
            json!({
                "type": "json_schema",
                "json_schema": {"name": "response", "schema": schema, "strict": true},
            }),
        );
    }
    Ok(Value::Object(body))
}

fn chat_messages(
    message: &ChatMessage,
    features: &ProviderFeatures,
    output: &mut Vec<Value>,
) -> Result<(), ProviderError> {
    let role = match message.role {
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
    };
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut reasoning = String::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: value } => text.push_str(value),
            ContentBlock::Reasoning { text: value, .. }
                if features.contains("reasoning_content") =>
            {
                reasoning.push_str(value);
            }
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": arguments.to_string()},
            })),
            ContentBlock::ToolResult {
                call_id, content, ..
            } if message.role == ChatRole::User => output.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": result_text(content)?,
            })),
            ContentBlock::Image { image } if message.role == ChatRole::User => {
                output.push(json!({
                    "role": "user",
                    "content": [{"type": "image_url", "image_url": {"url": media_url(&image.source)?}}],
                }));
            }
            ContentBlock::Reasoning { .. } => {}
            _ => return Err(invalid("unsupported Chat Completions content")),
        }
    }
    if !text.is_empty() || !tool_calls.is_empty() || !reasoning.is_empty() {
        let mut value = Map::new();
        value.insert("role".into(), json!(role));
        value.insert("content".into(), json!(text));
        if !tool_calls.is_empty() {
            value.insert("tool_calls".into(), Value::Array(tool_calls));
        }
        if !reasoning.is_empty() {
            value.insert("reasoning_content".into(), json!(reasoning));
        }
        output.push(Value::Object(value));
    }
    Ok(())
}

fn result_text(content: &[ToolResultContent]) -> Result<String, ProviderError> {
    let mut result = String::new();
    for item in content {
        match item {
            ToolResultContent::Text { text } => result.push_str(text),
            _ => return Err(invalid("protocol requires textual tool results")),
        }
    }
    Ok(result)
}

fn media_url(source: &MediaSource) -> Result<String, ProviderError> {
    match source {
        MediaSource::Url { url } => Ok(url.clone()),
        MediaSource::Base64 { media_type, data } => Ok(format!("data:{media_type};base64,{data}")),
    }
}

const fn effort_name(value: ReasoningEffort) -> &'static str {
    match value {
        ReasoningEffort::Disabled => "none",
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
    }
}

fn invalid(message: &'static str) -> ProviderError {
    error(
        ProviderErrorKind::InvalidRequest,
        ProviderErrorPhase::Open,
        message,
    )
}
