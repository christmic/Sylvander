use std::collections::BTreeMap;

use futures_util::StreamExt as _;
use reqwest::Response;
use serde_json::Value;
use sylvander_llm_core::{
    ContentBlock, ModelEventStream, ModelRef, ModelResponse, ModelStreamEvent, OpaqueProviderState,
    ProviderError, ProviderErrorKind, ProviderErrorPhase, StopReason, TokenUsage,
    TokenUsageDetails,
};

use crate::{OpenAiProtocol, error, transport};

pub(crate) fn events(
    response: Response,
    protocol: OpenAiProtocol,
    provider: String,
    expected_model: ModelRef,
) -> ModelEventStream {
    Box::pin(async_stream::try_stream! {
        let mut bytes = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut chat = Some(ChatState::new(expected_model));
        let mut completed = false;
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|source| transport(source, ProviderErrorPhase::Stream))?;
            buffer.extend_from_slice(&chunk);
            while let Some(end) = event_boundary(&buffer) {
                let raw = buffer.drain(..end).collect::<Vec<_>>();
                drain_separators(&mut buffer);
                let Some(data) = event_data(&raw)? else {
                    continue;
                };
                if data == "[DONE]" {
                    if protocol == OpenAiProtocol::ChatCompletions && !completed {
                        completed = true;
                        let state = chat.take().ok_or_else(|| protocol_error("chat stream completed twice"))?;
                        yield ModelStreamEvent::Completed(Box::new(state.finish()?));
                    }
                    continue;
                }
                let value: Value = serde_json::from_str(&data).map_err(|_| protocol_error("provider emitted invalid SSE JSON"))?;
                match protocol {
                    OpenAiProtocol::Responses => {
                        if let Some(event) = response_event(&provider, value)? {
                            if matches!(event, ModelStreamEvent::Completed(_)) {
                                completed = true;
                            }
                            yield event;
                        }
                    }
                    OpenAiProtocol::ChatCompletions => {
                        let state = chat.as_mut().ok_or_else(|| protocol_error("chat event followed completion"))?;
                        for event in state.push(&value) {
                            yield event;
                        }
                    }
                }
            }
        }
        if !completed {
            if protocol == OpenAiProtocol::ChatCompletions
                && chat.as_ref().is_some_and(|state| state.saw_chunk)
            {
                let state = chat.take().ok_or_else(|| protocol_error("chat stream state is missing"))?;
                yield ModelStreamEvent::Completed(Box::new(state.finish()?));
            } else {
                Err(protocol_error("provider stream ended before completion"))?;
            }
        }
    })
}

fn event_boundary(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|value| value == b"\n\n")
        .map(|index| index + 2)
        .or_else(|| {
            buffer
                .windows(4)
                .position(|value| value == b"\r\n\r\n")
                .map(|index| index + 4)
        })
}

fn drain_separators(buffer: &mut Vec<u8>) {
    while matches!(buffer.first(), Some(b'\n' | b'\r')) {
        buffer.remove(0);
    }
}

fn event_data(raw: &[u8]) -> Result<Option<String>, ProviderError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| protocol_error("provider emitted non-UTF-8 SSE data"))?;
    let data = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>();
    Ok((!data.is_empty()).then(|| data.join("\n")))
}

fn response_event(provider: &str, value: Value) -> Result<Option<ModelStreamEvent>, ProviderError> {
    match value.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta" | "response.text.delta") => Ok(value
            .get("delta")
            .and_then(Value::as_str)
            .map(|text| ModelStreamEvent::TextDelta(text.into()))),
        Some("response.reasoning_text.delta" | "response.reasoning_summary_text.delta") => {
            Ok(value
                .get("delta")
                .and_then(Value::as_str)
                .map(|text| ModelStreamEvent::ReasoningDelta(text.into())))
        }
        Some("response.completed") => {
            let response = value
                .get("response")
                .ok_or_else(|| protocol_error("completed event has no response"))?;
            Ok(Some(ModelStreamEvent::Completed(Box::new(parse_response(
                provider, response,
            )?))))
        }
        Some("error" | "response.failed") => {
            Err(protocol_error("provider reported a streaming failure"))
        }
        _ => Ok(None),
    }
}

fn parse_response(provider: &str, value: &Value) -> Result<ModelResponse, ProviderError> {
    let id = string(value, "id")?;
    let model = string(value, "model")?;
    let mut content = Vec::new();
    for item in array(value, "output")? {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in array(item, "content")? {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => content.push(ContentBlock::Text {
                            text: string(part, "text")?.into(),
                        }),
                        Some("refusal") => content.push(ContentBlock::Text {
                            text: string(part, "refusal")?.into(),
                        }),
                        _ => {}
                    }
                }
            }
            Some("function_call") => content.push(ContentBlock::ToolCall {
                id: string(item, "call_id")?.into(),
                name: string(item, "name")?.into(),
                arguments: serde_json::from_str(string(item, "arguments")?)
                    .map_err(|_| protocol_error("tool arguments are invalid JSON"))?,
            }),
            Some("reasoning") => {
                let text = item
                    .get("summary")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<String>();
                content.push(ContentBlock::Reasoning {
                    text,
                    opaque_state: Some(OpaqueProviderState {
                        provider: provider.into(),
                        data: item.clone(),
                    }),
                });
            }
            _ => {}
        }
    }
    let status = value.get("status").and_then(Value::as_str);
    let incomplete = value
        .get("incomplete_details")
        .and_then(|item| item.get("reason"))
        .and_then(Value::as_str);
    let stop_reason = if content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolCall { .. }))
    {
        StopReason::ToolUse
    } else if incomplete == Some("max_output_tokens") {
        StopReason::MaxOutputTokens
    } else if incomplete == Some("content_filter") {
        StopReason::Refusal
    } else if status == Some("completed") {
        StopReason::EndTurn
    } else {
        StopReason::Other(status.unwrap_or("unknown").into())
    };
    Ok(ModelResponse {
        id: id.into(),
        model: ModelRef::new(provider, model),
        content,
        stop_reason,
        usage: response_usage(value.get("usage")),
    })
}

struct ChatToolCall {
    id: String,
    name: String,
    arguments: String,
}

struct ChatState {
    id: String,
    model: ModelRef,
    content: String,
    reasoning: String,
    refusal: String,
    tool_calls: BTreeMap<u64, ChatToolCall>,
    finish_reason: Option<String>,
    usage: TokenUsage,
    saw_chunk: bool,
}

impl ChatState {
    fn new(model: ModelRef) -> Self {
        Self {
            id: String::new(),
            model,
            content: String::new(),
            reasoning: String::new(),
            refusal: String::new(),
            tool_calls: BTreeMap::new(),
            finish_reason: None,
            usage: TokenUsage::default(),
            saw_chunk: false,
        }
    }

    fn push(&mut self, value: &Value) -> Vec<ModelStreamEvent> {
        self.saw_chunk = true;
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            self.id = id.into();
        }
        if let Some(model) = value.get("model").and_then(Value::as_str) {
            self.model.model = model.into();
        }
        if value.get("usage").is_some_and(|usage| !usage.is_null()) {
            self.usage = chat_usage(value.get("usage"));
        }
        let mut events = Vec::new();
        for choice in value
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish_reason = Some(reason.into());
            }
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                self.content.push_str(text);
                events.push(ModelStreamEvent::TextDelta(text.into()));
            }
            if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str) {
                self.reasoning.push_str(text);
                events.push(ModelStreamEvent::ReasoningDelta(text.into()));
            }
            if let Some(text) = delta.get("refusal").and_then(Value::as_str) {
                self.refusal.push_str(text);
            }
            for tool in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let index = tool.get("index").and_then(Value::as_u64).unwrap_or(0);
                let entry = self
                    .tool_calls
                    .entry(index)
                    .or_insert_with(|| ChatToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                if let Some(id) = tool.get("id").and_then(Value::as_str) {
                    entry.id.push_str(id);
                }
                if let Some(function) = tool.get("function") {
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        entry.name.push_str(name);
                    }
                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                        entry.arguments.push_str(arguments);
                    }
                }
            }
        }
        events
    }

    fn finish(self) -> Result<ModelResponse, ProviderError> {
        let mut content = Vec::new();
        if !self.reasoning.is_empty() {
            content.push(ContentBlock::Reasoning {
                text: self.reasoning,
                opaque_state: None,
            });
        }
        if !self.content.is_empty() {
            content.push(ContentBlock::Text { text: self.content });
        }
        if !self.refusal.is_empty() {
            content.push(ContentBlock::Text { text: self.refusal });
        }
        for (_, tool) in self.tool_calls {
            content.push(ContentBlock::ToolCall {
                id: tool.id,
                name: tool.name,
                arguments: serde_json::from_str(&tool.arguments)
                    .map_err(|_| protocol_error("tool arguments are invalid JSON"))?,
            });
        }
        let stop_reason = match self.finish_reason.as_deref() {
            Some("tool_calls" | "function_call") => StopReason::ToolUse,
            Some("length") => StopReason::MaxOutputTokens,
            Some("content_filter") => StopReason::Refusal,
            Some("stop") | None => StopReason::EndTurn,
            Some(value) => StopReason::Other(value.into()),
        };
        Ok(ModelResponse {
            id: self.id,
            model: self.model,
            content,
            stop_reason,
            usage: self.usage,
        })
    }
}

fn response_usage(value: Option<&Value>) -> TokenUsage {
    let value = value.unwrap_or(&Value::Null);
    let details = value.get("input_tokens_details").unwrap_or(&Value::Null);
    let output_details = value.get("output_tokens_details").unwrap_or(&Value::Null);
    TokenUsage {
        input_tokens: number(value, "input_tokens"),
        output_tokens: number(value, "output_tokens"),
        cache_write_tokens: optional_number(details, "cache_write_tokens"),
        cache_read_tokens: optional_number(details, "cached_tokens"),
        details: TokenUsageDetails {
            reported_total_tokens: optional_number(value, "total_tokens"),
            reasoning_tokens: optional_number(output_details, "reasoning_tokens"),
            ..TokenUsageDetails::default()
        },
    }
}

fn chat_usage(value: Option<&Value>) -> TokenUsage {
    let value = value.unwrap_or(&Value::Null);
    let prompt = value.get("prompt_tokens_details").unwrap_or(&Value::Null);
    let completion = value
        .get("completion_tokens_details")
        .unwrap_or(&Value::Null);
    TokenUsage {
        input_tokens: number(value, "prompt_tokens"),
        output_tokens: number(value, "completion_tokens"),
        cache_write_tokens: optional_number(prompt, "cache_write_tokens"),
        cache_read_tokens: optional_number(prompt, "cached_tokens"),
        details: TokenUsageDetails {
            reported_total_tokens: optional_number(value, "total_tokens"),
            reasoning_tokens: optional_number(completion, "reasoning_tokens"),
            audio_input_tokens: optional_number(prompt, "audio_tokens"),
            audio_output_tokens: optional_number(completion, "audio_tokens"),
            accepted_prediction_tokens: optional_number(completion, "accepted_prediction_tokens"),
            rejected_prediction_tokens: optional_number(completion, "rejected_prediction_tokens"),
            ..TokenUsageDetails::default()
        },
    }
}

fn string<'a>(value: &'a Value, key: &'static str) -> Result<&'a str, ProviderError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_error("provider response is missing a required string"))
}

fn array<'a>(value: &'a Value, key: &'static str) -> Result<&'a [Value], ProviderError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| protocol_error("provider response is missing a required array"))
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn optional_number(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn protocol_error(message: &'static str) -> ProviderError {
    error(
        ProviderErrorKind::Protocol,
        ProviderErrorPhase::Stream,
        message,
    )
}
