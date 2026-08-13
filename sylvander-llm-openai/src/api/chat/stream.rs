//! Chat Completions chunk assembly following the official SDK stream shape.

use std::collections::BTreeMap;

use futures_util::StreamExt as _;
use reqwest::Response;
use serde_json::Value;

use crate::api::OpenAiError;
use crate::api::chat::{ChatCompletion, ChatCompletionChunk, ChatCompletionUsage, ChatToolCall};
use crate::api::sse::SseParser;

#[derive(Debug, Clone, PartialEq)]
pub enum ChatStreamEvent {
    ContentDelta(String),
    ReasoningDelta(String),
    Completed(Box<ChatCompletion>),
}

pub struct ChatCompletionStream {
    inner: PinStream,
}

type PinStream = std::pin::Pin<
    Box<dyn futures_util::Stream<Item = Result<ChatStreamEvent, OpenAiError>> + Send>,
>;

impl ChatCompletionStream {
    pub(crate) fn new(response: Response) -> Self {
        let stream = async_stream::try_stream! {
            let mut body = response.bytes_stream();
            let mut parser = SseParser::default();
            let mut state = Some(ChatState::default());
            let mut terminal = false;
            while let Some(chunk) = body.next().await {
                for event in parser.feed(&chunk?) {
                    let event = event?;
                    if event.data == "[DONE]" {
                        if terminal {
                            Err(OpenAiError::Protocol("Chat stream emitted [DONE] twice".into()))?;
                        }
                        terminal = true;
                        let completed = state
                            .take()
                            .ok_or_else(|| OpenAiError::Protocol("Chat stream state is missing".into()))?
                            .finish()?;
                        yield ChatStreamEvent::Completed(Box::new(completed));
                        continue;
                    }
                    if terminal {
                        Err(OpenAiError::Protocol("Chat chunk followed [DONE]".into()))?;
                    }
                    let value: Value = serde_json::from_str(&event.data)?;
                    if value.get("error").is_some() {
                        Err(OpenAiError::Protocol("Chat stream emitted an error payload".into()))?;
                    }
                    let chunk: ChatCompletionChunk = serde_json::from_value(value)?;
                    let current = state
                        .as_mut()
                        .ok_or_else(|| OpenAiError::Protocol("Chat chunk followed completion".into()))?;
                    for decoded in current.push(chunk)? {
                        yield decoded;
                    }
                }
            }
            parser.finish()?;
            if !terminal {
                let current = state
                    .take()
                    .ok_or_else(|| OpenAiError::Protocol("Chat stream state is missing".into()))?;
                if !current.has_compatible_terminal_evidence() {
                    Err(OpenAiError::Protocol("Chat stream ended before [DONE] or a complete usage tail".into()))?;
                }
                yield ChatStreamEvent::Completed(Box::new(current.finish()?));
            }
        };
        Self {
            inner: Box::pin(stream),
        }
    }
}

impl futures_util::Stream for ChatCompletionStream {
    type Item = Result<ChatStreamEvent, OpenAiError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}

#[derive(Default)]
struct ChatState {
    id: Option<String>,
    model: Option<String>,
    content: String,
    refusal: String,
    reasoning: String,
    tools: BTreeMap<u64, ToolState>,
    finish_reason: Option<String>,
    usage: Option<ChatCompletionUsage>,
}

#[derive(Default)]
struct ToolState {
    id: String,
    name: String,
    arguments: String,
}

impl ChatState {
    fn has_compatible_terminal_evidence(&self) -> bool {
        self.finish_reason.is_some() && self.usage.is_some()
    }

    fn push(&mut self, chunk: ChatCompletionChunk) -> Result<Vec<ChatStreamEvent>, OpenAiError> {
        self.id = Some(chunk.id);
        self.model = Some(chunk.model);
        if chunk.usage.is_some() {
            self.usage = chunk.usage;
        }
        let mut output = Vec::new();
        for choice in chunk.choices {
            if choice.index != 0 {
                return Err(OpenAiError::Protocol(
                    "Chat stream returned an unrequested additional choice".into(),
                ));
            }
            if let Some(reason) = choice.finish_reason {
                self.finish_reason = Some(reason);
            }
            if let Some(text) = choice.delta.content {
                self.content.push_str(&text);
                output.push(ChatStreamEvent::ContentDelta(text));
            }
            if let Some(text) = choice.delta.reasoning_content {
                self.reasoning.push_str(&text);
                output.push(ChatStreamEvent::ReasoningDelta(text));
            }
            if let Some(text) = choice.delta.refusal {
                self.refusal.push_str(&text);
            }
            for tool in choice.delta.tool_calls.unwrap_or_default() {
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
        Ok(output)
    }

    fn finish(self) -> Result<ChatCompletion, OpenAiError> {
        let finish_reason = self
            .finish_reason
            .ok_or_else(|| OpenAiError::Protocol("Chat stream has no finish reason".into()))?;
        let tools = self
            .tools
            .into_values()
            .map(|tool| ChatToolCall {
                id: tool.id,
                name: tool.name,
                arguments: tool.arguments,
            })
            .collect();
        Ok(ChatCompletion {
            id: self
                .id
                .ok_or_else(|| OpenAiError::Protocol("Chat stream has no id".into()))?,
            model: self
                .model
                .ok_or_else(|| OpenAiError::Protocol("Chat stream has no model".into()))?,
            content: self.content,
            refusal: self.refusal,
            reasoning_content: self.reasoning,
            tool_calls: tools,
            finish_reason,
            usage: self.usage,
        })
    }
}
