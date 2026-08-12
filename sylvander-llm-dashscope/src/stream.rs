use std::collections::BTreeMap;

use futures_util::StreamExt as _;
use reqwest::Response;
use serde_json::Value;
use sylvander_llm_core::{
    ContentBlock, ModelEventStream, ModelRef, ModelResponse, ModelStreamEvent, ProviderError,
    ProviderErrorKind, ProviderErrorPhase, StopReason, TokenUsage, TokenUsageDetails,
};

use crate::{error, transport};

pub(crate) fn events(response: Response, _provider: String, model: ModelRef) -> ModelEventStream {
    Box::pin(async_stream::try_stream! {
        let mut bytes = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut state = Some(State::new(model));
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
                let value: Value = serde_json::from_str(&data)
                    .map_err(|_| protocol_error("provider emitted invalid SSE JSON"))?;
                let current = state.as_mut().ok_or_else(|| protocol_error("event followed completion"))?;
                for event in current.push(&value)? {
                    yield event;
                }
                if current.finished() {
                    completed = true;
                    let final_state = state.take().ok_or_else(|| protocol_error("stream state is missing"))?;
                    yield ModelStreamEvent::Completed(final_state.finish()?);
                }
            }
        }
        if !completed {
            let final_state = state.take().ok_or_else(|| protocol_error("stream state is missing"))?;
            if final_state.saw_chunk {
                yield ModelStreamEvent::Completed(final_state.finish()?);
            } else {
                Err(protocol_error("provider stream ended before output"))?;
            }
        }
    })
}

struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

struct State {
    request_id: String,
    model: ModelRef,
    content: String,
    reasoning: String,
    tools: BTreeMap<u64, ToolCall>,
    finish_reason: Option<String>,
    usage: TokenUsage,
    saw_chunk: bool,
}

impl State {
    fn new(model: ModelRef) -> Self {
        Self {
            request_id: String::new(),
            model,
            content: String::new(),
            reasoning: String::new(),
            tools: BTreeMap::new(),
            finish_reason: None,
            usage: TokenUsage::default(),
            saw_chunk: false,
        }
    }

    fn push(&mut self, value: &Value) -> Result<Vec<ModelStreamEvent>, ProviderError> {
        self.saw_chunk = true;
        if let Some(id) = value.get("request_id").and_then(Value::as_str) {
            self.request_id = id.into();
        }
        if let Some(usage) = value.get("usage") {
            self.usage = TokenUsage {
                input_tokens: number(usage, "input_tokens"),
                output_tokens: number(usage, "output_tokens"),
                details: TokenUsageDetails {
                    reported_total_tokens: usage.get("total_tokens").and_then(Value::as_u64),
                    ..TokenUsageDetails::default()
                },
                ..TokenUsage::default()
            };
        }
        let mut events = Vec::new();
        let output = value.get("output").unwrap_or(&Value::Null);
        if let Some(text) = output
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            self.content.push_str(text);
            events.push(ModelStreamEvent::TextDelta(text.into()));
        }
        for choice in output
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish_reason = Some(reason.into());
            }
            let message = choice.get("message").unwrap_or(&Value::Null);
            if let Some(text) = message
                .get("content")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                self.content.push_str(text);
                events.push(ModelStreamEvent::TextDelta(text.into()));
            }
            if let Some(text) = message
                .get("reasoning_content")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                self.reasoning.push_str(text);
                events.push(ModelStreamEvent::ReasoningDelta(text.into()));
            }
            for tool in message
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let index = tool.get("index").and_then(Value::as_u64).unwrap_or(0);
                let entry = self.tools.entry(index).or_insert_with(|| ToolCall {
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
        if self.finish_reason.is_none()
            && let Some(reason) = output.get("finish_reason").and_then(Value::as_str)
        {
            self.finish_reason = Some(reason.into());
        }
        Ok(events)
    }

    fn finished(&self) -> bool {
        self.finish_reason.is_some()
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
        for (_, tool) in self.tools {
            content.push(ContentBlock::ToolCall {
                id: tool.id,
                name: tool.name,
                arguments: serde_json::from_str(&tool.arguments)
                    .map_err(|_| protocol_error("tool arguments are invalid JSON"))?,
            });
        }
        let stop_reason = match self.finish_reason.as_deref() {
            Some("tool_calls") => StopReason::ToolUse,
            Some("length") => StopReason::MaxOutputTokens,
            Some("stop") | None => StopReason::EndTurn,
            Some(value) => StopReason::Other(value.into()),
        };
        Ok(ModelResponse {
            id: self.request_id,
            model: self.model,
            content,
            stop_reason,
            usage: self.usage,
        })
    }
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

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn protocol_error(message: &'static str) -> ProviderError {
    error(
        ProviderErrorKind::Protocol,
        ProviderErrorPhase::Stream,
        message,
    )
}
