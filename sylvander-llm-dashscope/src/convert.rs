use serde_json::{Map, Value, json};
use sylvander_llm_core::{
    ChatMessage, ChatRole, ContentBlock, MediaSource, ModelRequest, ProviderError,
    ProviderErrorKind, ProviderErrorPhase, ToolResultContent,
};

use crate::{DashScopeFeatures, error};

pub(crate) fn request(
    features: &DashScopeFeatures,
    input: &ModelRequest,
) -> Result<Value, ProviderError> {
    let mut messages = input
        .system
        .iter()
        .map(|instruction| json!({"role": "system", "content": instruction.text}))
        .collect::<Vec<_>>();
    for message in &input.messages {
        append_message(message, features, &mut messages)?;
    }
    let mut parameters = Map::new();
    parameters.insert("result_format".into(), json!("message"));
    parameters.insert("incremental_output".into(), Value::Bool(true));
    parameters.insert("max_tokens".into(), json!(input.max_output_tokens));
    if let Some(reasoning) = input.reasoning {
        if features.contains("enable_thinking") {
            parameters.insert("enable_thinking".into(), Value::Bool(true));
        }
        if features.contains("thinking_budget")
            && let Some(budget) = reasoning.budget_tokens
        {
            parameters.insert("thinking_budget".into(), json!(budget));
        }
    }
    if features.contains("parallel_tool_calls") && !input.tools.is_empty() {
        parameters.insert("parallel_tool_calls".into(), Value::Bool(true));
    }
    if let Some(schema) = &input.output_schema
        && features.contains("response_format")
    {
        parameters.insert(
            "response_format".into(),
            json!({"type": "json_object", "schema": schema}),
        );
    }
    if !input.tools.is_empty() {
        parameters.insert(
            "tools".into(),
            Value::Array(
                input
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
                    .collect(),
            ),
        );
    }
    Ok(json!({
        "model": input.model.model,
        "input": {"messages": messages},
        "parameters": parameters,
    }))
}

fn append_message(
    message: &ChatMessage,
    features: &DashScopeFeatures,
    output: &mut Vec<Value>,
) -> Result<(), ProviderError> {
    let role = match message.role {
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
    };
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
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
                    "content": [{"image": media_url(&image.source)?}],
                }));
            }
            ContentBlock::Reasoning { .. } => {}
            _ => return Err(invalid("unsupported DashScope message content")),
        }
    }
    if !text.is_empty() || !reasoning.is_empty() || !tool_calls.is_empty() {
        let mut value = Map::new();
        value.insert("role".into(), json!(role));
        value.insert("content".into(), json!(text));
        if !reasoning.is_empty() {
            value.insert("reasoning_content".into(), json!(reasoning));
        }
        if !tool_calls.is_empty() {
            value.insert("tool_calls".into(), Value::Array(tool_calls));
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
            _ => {
                return Err(invalid(
                    "DashScope Generation requires textual tool results",
                ));
            }
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

fn invalid(message: &'static str) -> ProviderError {
    error(
        ProviderErrorKind::InvalidRequest,
        ProviderErrorPhase::Open,
        message,
    )
}
