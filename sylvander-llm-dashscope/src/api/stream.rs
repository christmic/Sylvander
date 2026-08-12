//! Native Generation stream decoding and strict completion assembly.

use std::collections::BTreeMap;

use futures_util::StreamExt as _;
use reqwest::Response;

use crate::api::sse::SseParser;
use crate::api::types::GenerationResponse;
use crate::api::{DashScopeError, GenerationCompletion, GenerationToolCall, GenerationUsage};

#[derive(Debug, Clone, PartialEq)]
pub enum GenerationStreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
    Completed(GenerationCompletion),
}

pub struct GenerationStream {
    inner: PinStream,
}

type PinStream = std::pin::Pin<
    Box<dyn futures_util::Stream<Item = Result<GenerationStreamEvent, DashScopeError>> + Send>,
>;

impl GenerationStream {
    pub(crate) fn new(response: Response) -> Self {
        let stream = async_stream::try_stream! {
            let mut body = response.bytes_stream();
            let mut parser = SseParser::default();
            let mut state = Some(State::default());
            let mut done = false;
            while let Some(chunk) = body.next().await {
                for event in parser.feed(&chunk?) {
                    let event = event?;
                    if event.kind.as_deref() == Some("error") {
                        let error: GenerationResponse = serde_json::from_str(&event.data)?;
                        Err(DashScopeError::Api {
                            status: event.status.unwrap_or(400),
                            code: error.code.unwrap_or_else(|| "Unknown".into()),
                            request_id: error.request_id,
                        })?;
                    }
                    if event.kind.as_deref() == Some("done") || event.data == "[DONE]" {
                        done = true;
                        continue;
                    }
                    if done {
                        Err(DashScopeError::Protocol("Generation data followed done event".into()))?;
                    }
                    let response: GenerationResponse = serde_json::from_str(&event.data)?;
                    if let Some(code) = response.code.clone() {
                        Err(DashScopeError::Api {
                            status: event.status.unwrap_or(400),
                            code,
                            request_id: response.request_id.clone(),
                        })?;
                    }
                    let current = state
                        .as_mut()
                        .ok_or_else(|| DashScopeError::Protocol("Generation state is missing".into()))?;
                    for decoded in current.push(response)? {
                        yield decoded;
                    }
                }
            }
            parser.finish()?;
            let completion = state
                .take()
                .ok_or_else(|| DashScopeError::Protocol("Generation state is missing".into()))?
                .finish()?;
            yield GenerationStreamEvent::Completed(completion);
        };
        Self {
            inner: Box::pin(stream),
        }
    }
}

impl futures_util::Stream for GenerationStream {
    type Item = Result<GenerationStreamEvent, DashScopeError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}

#[derive(Default)]
struct State {
    request_id: Option<String>,
    content: String,
    reasoning: String,
    tools: BTreeMap<u64, ToolState>,
    finish_reason: Option<String>,
    usage: Option<GenerationUsage>,
    saw_response: bool,
}

#[derive(Default)]
struct ToolState {
    id: String,
    name: String,
    arguments: String,
}

impl State {
    fn push(
        &mut self,
        response: GenerationResponse,
    ) -> Result<Vec<GenerationStreamEvent>, DashScopeError> {
        self.saw_response = true;
        if response.request_id.is_some() {
            self.request_id = response.request_id;
        }
        if response.usage.is_some() {
            self.usage = response.usage;
        }
        let mut events = Vec::new();
        let Some(output) = response.output else {
            return Ok(events);
        };
        if let Some(text) = output.text.filter(|text| !text.is_empty()) {
            self.content.push_str(&text);
            events.push(GenerationStreamEvent::TextDelta(text));
        }
        if output.finish_reason.is_some() {
            self.finish_reason = output.finish_reason;
        }
        for choice in output.choices {
            if choice.index.unwrap_or(0) != 0 {
                return Err(DashScopeError::Protocol(
                    "Generation returned an unrequested additional choice".into(),
                ));
            }
            if choice.finish_reason.is_some() {
                self.finish_reason = choice.finish_reason;
            }
            let Some(message) = choice.message else {
                continue;
            };
            if let Some(text) = message.content.filter(|text| !text.is_empty()) {
                self.content.push_str(&text);
                events.push(GenerationStreamEvent::TextDelta(text));
            }
            if let Some(text) = message.reasoning_content.filter(|text| !text.is_empty()) {
                self.reasoning.push_str(&text);
                events.push(GenerationStreamEvent::ReasoningDelta(text));
            }
            for tool in message.tool_calls.unwrap_or_default() {
                let state = self.tools.entry(tool.index).or_default();
                if let Some(id) = tool.id {
                    state.id.push_str(&id);
                }
                if let Some(function) = tool.function {
                    if let Some(name) = function.name {
                        state.name.push_str(&name);
                    }
                    if let Some(arguments) = function.arguments {
                        state.arguments.push_str(&arguments);
                    }
                }
            }
        }
        Ok(events)
    }

    fn finish(self) -> Result<GenerationCompletion, DashScopeError> {
        if !self.saw_response {
            return Err(DashScopeError::Protocol(
                "Generation stream ended before output".into(),
            ));
        }
        let finish_reason = self.finish_reason.ok_or_else(|| {
            DashScopeError::Protocol("Generation stream has no finish reason".into())
        })?;
        let usage = self.usage.ok_or_else(|| {
            DashScopeError::Protocol("Generation stream completed without usage".into())
        })?;
        Ok(GenerationCompletion {
            request_id: self.request_id.unwrap_or_default(),
            content: self.content,
            reasoning_content: self.reasoning,
            tool_calls: self
                .tools
                .into_values()
                .map(|tool| GenerationToolCall {
                    id: tool.id,
                    name: tool.name,
                    arguments: tool.arguments,
                })
                .collect(),
            finish_reason,
            usage,
        })
    }
}
