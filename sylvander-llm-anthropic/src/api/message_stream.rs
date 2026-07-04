//! `MessageStream` — `impl Stream<Item = Result<RawStreamEvent, AnthropicError>>` wrapper
//! around an Anthropic streaming response.
//!
//! Maintains internal state so callers can call [`MessageStream::final_message`]
//! after the stream completes to get the fully assembled [`Message`].
//!
//! See [`crate::api::streaming::SseParser`] for the underlying byte-level
//! SSE parser.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::stream::Stream;
use reqwest::Response;
use serde_json::Value as JsonValue;

use crate::api::error::AnthropicError;
use crate::api::streaming::SseParser;
use crate::api::types::{
    ContentBlock, Message, MessageDeltaUsage, RawStreamEvent, StopReason, TextBlock, ThinkingBlock,
    ToolUseBlock, Usage,
};

/// Streaming response from `POST /v1/messages` (with `stream: true`).
///
/// Yields raw stream events. After the stream completes, call
/// [`final_message`](Self::final_message) to retrieve the assembled
/// [`Message`].
pub struct MessageStream {
    /// Underlying HTTP response body byte stream.
    body: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    /// SSE byte parser.
    parser: SseParser,
    /// Buffered events not yet yielded.
    pending: Vec<Result<RawStreamEvent, AnthropicError>>,
    /// Accumulated state for `final_message()`.
    state: Arc<Mutex<MessageStreamState>>,
    /// Whether the underlying stream has ended.
    body_done: bool,
}

/// Internal mutable state shared with the stream consumer.
#[derive(Debug, Default)]
struct MessageStreamState {
    /// Initial message from `message_start`.
    message: Option<Message>,
    /// Per-index text accumulator for text blocks.
    text_accum: HashMap<u32, String>,
    /// Per-index `tool_use` input accumulator (raw JSON string per index).
    tool_input_accum: HashMap<u32, String>,
    /// Per-index thinking accumulator for thinking blocks.
    thinking_accum: HashMap<u32, String>,
    /// Per-index signature accumulator for thinking blocks.
    signature_accum: HashMap<u32, String>,
    /// Per-index initial content block (so we can rebuild `ToolUseBlock` with
    /// assembled input).
    initial_blocks: HashMap<u32, ContentBlock>,
    /// Order in which blocks were started (for final assembly ordering).
    block_order: Vec<u32>,
    /// Final `stop_reason`.
    stop_reason: Option<StopReason>,
    /// Final cumulative usage.
    usage: Option<Usage>,
    /// Whether `message_stop` was seen.
    finished: bool,
}

impl MessageStream {
    /// Construct a `MessageStream` from an HTTP response that has a
    /// `text/event-stream` body. Internal — callers obtain this via
    /// [`crate::api::messages::MessagesApi::stream`].
    pub(crate) fn new(response: Response) -> Self {
        let body = response.bytes_stream();
        Self {
            body: Box::pin(body),
            parser: SseParser::new(),
            pending: Vec::new(),
            state: Arc::new(Mutex::new(MessageStreamState::default())),
            body_done: false,
        }
    }

    /// Get the assembled final [`Message`]. Available after `message_stop`
    /// has been observed.
    ///
    /// Returns `None` if the stream hasn't completed yet.
    #[must_use]
    pub fn final_message(&self) -> Option<Message> {
        let state = self.state.lock().ok()?;
        if !state.finished {
            return None;
        }
        let mut msg = state.message.clone()?;
        // Rebuild content blocks with accumulated text / tool inputs.
        let mut content = Vec::with_capacity(state.block_order.len());
        for &index in &state.block_order {
            let Some(initial) = state.initial_blocks.get(&index) else {
                continue;
            };
            match initial {
                ContentBlock::Text(_) => {
                    let text = state.text_accum.get(&index).cloned().unwrap_or_default();
                    content.push(ContentBlock::Text(TextBlock::new(text)));
                }
                ContentBlock::ToolUse(_) => {
                    let input_str = state
                        .tool_input_accum
                        .get(&index)
                        .cloned()
                        .unwrap_or_default();
                    let input: JsonValue =
                        serde_json::from_str(&input_str).unwrap_or(JsonValue::Null);
                    if let ContentBlock::ToolUse(tu) = initial {
                        content.push(ContentBlock::ToolUse(ToolUseBlock::new(
                            tu.id.clone(),
                            tu.name.clone(),
                            input,
                        )));
                    }
                }
                ContentBlock::Thinking(_) => {
                    let thinking = state
                        .thinking_accum
                        .get(&index)
                        .cloned()
                        .unwrap_or_default();
                    let signature = state
                        .signature_accum
                        .get(&index)
                        .cloned()
                        .unwrap_or_default();
                    content.push(ContentBlock::Thinking(ThinkingBlock::new(
                        thinking,
                        signature,
                    )));
                }
            }
        }
        msg.content = content;
        msg.stop_reason = state.stop_reason;
        if let Some(usage) = &state.usage {
            msg.usage = usage.clone();
        }
        Some(msg)
    }

    /// Current cumulative usage, if a `message_delta` event has been
    /// observed.
    #[must_use]
    pub fn usage(&self) -> Option<MessageDeltaUsage> {
        let state = self.state.lock().ok()?;
        state.usage.as_ref().map(|u| MessageDeltaUsage {
            input_tokens: Some(u.input_tokens),
            output_tokens: u.output_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
        })
    }
}

impl Stream for MessageStream {
    type Item = Result<RawStreamEvent, AnthropicError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Drain pending events first.
        if !self.pending.is_empty() {
            return Poll::Ready(Some(self.pending.remove(0)));
        }

        if self.body_done {
            return Poll::Ready(None);
        }

        // Pull the next chunk from the HTTP body.
        loop {
            match self.body.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    let events = self.parser.feed(&bytes);
                    if events.is_empty() {
                        // No complete events yet — pull another chunk.
                        continue;
                    }
                    // Apply side effects to state before yielding.
                    for ev in events.iter().flatten() {
                        apply_event(&self.state, ev);
                    }
                    self.pending.extend(events);
                    return Poll::Ready(Some(self.pending.remove(0)));
                }
                Poll::Ready(Some(Err(e))) => {
                    self.body_done = true;
                    return Poll::Ready(Some(Err(AnthropicError::Http(e))));
                }
                Poll::Ready(None) => {
                    self.body_done = true;
                    // Validate no unfinished event remains. The parser's
                    // buffer state is owned; we just take it via `mem::replace`
                    // with an empty parser to avoid moving out of `self`.
                    let mut empty_parser = SseParser::new();
                    std::mem::swap(&mut self.parser, &mut empty_parser);
                    if let Err(e) = empty_parser.finish() {
                        return Poll::Ready(Some(Err(e)));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Apply an event's side effects to the stream state.
fn apply_event(state: &Arc<Mutex<MessageStreamState>>, event: &RawStreamEvent) {
    let Ok(mut s) = state.lock() else {
        return;
    };
    match event {
        RawStreamEvent::MessageStart { message } => {
            s.message = Some(message.clone());
        }
        RawStreamEvent::ContentBlockStart {
            index,
            content_block,
        } => {
            s.initial_blocks.insert(*index, content_block.clone());
            if !s.block_order.contains(index) {
                s.block_order.push(*index);
            }
        }
        RawStreamEvent::ContentBlockDelta { index, delta } => match delta {
            crate::api::types::ContentDelta::TextDelta { text } => {
                s.text_accum.entry(*index).or_default().push_str(text);
            }
            crate::api::types::ContentDelta::InputJsonDelta { partial_json } => {
                s.tool_input_accum
                    .entry(*index)
                    .or_default()
                    .push_str(partial_json);
            }
            crate::api::types::ContentDelta::ThinkingDelta { thinking } => {
                s.thinking_accum
                    .entry(*index)
                    .or_default()
                    .push_str(thinking);
            }
            crate::api::types::ContentDelta::SignatureDelta { signature } => {
                s.signature_accum
                    .entry(*index)
                    .or_default()
                    .push_str(signature);
            }
            crate::api::types::ContentDelta::CitationsDelta { .. } => {
                // Citations ignored.
            }
        },
        RawStreamEvent::MessageDelta { delta, usage } => {
            s.stop_reason = delta.stop_reason;
            s.usage = Some(Usage {
                input_tokens: usage.input_tokens.unwrap_or(0),
                output_tokens: usage.output_tokens,
                cache_creation_input_tokens: usage.cache_creation_input_tokens,
                cache_read_input_tokens: usage.cache_read_input_tokens,
            });
        }
        RawStreamEvent::MessageStop => {
            s.finished = true;
        }
        RawStreamEvent::Ping | RawStreamEvent::ContentBlockStop { .. } => {
            // No state side-effects.
        }
    }
}