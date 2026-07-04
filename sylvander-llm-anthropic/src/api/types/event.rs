//! SSE stream event types — the wire format returned by
//! `POST /v1/messages` with `stream: true`.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::block::ContentBlock;
use super::message::Message;
use super::stop_reason::StopReason;

/// All seven SSE event types emitted by the Messages API.
///
/// Discriminated by the `type` field on the outer event object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RawStreamEvent {
    /// `message_start` — emitted once at the start of a stream.
    /// Carries the initial Message object (with empty content).
    #[serde(rename = "message_start")]
    MessageStart {
        /// The initial Message (with empty content).
        message: Message,
    },
    /// `content_block_start` — emitted when a new content block begins.
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        /// Zero-based index of the content block.
        index: u32,
        /// The initial content block (usually empty for non-text blocks).
        content_block: ContentBlock,
    },
    /// `ping` — keep-alive event. Can be ignored by callers.
    #[serde(rename = "ping")]
    Ping,
    /// `content_block_delta` — incremental update to a content block.
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        /// Zero-based index of the content block.
        index: u32,
        /// The delta payload.
        delta: ContentDelta,
    },
    /// `content_block_stop` — emitted when a content block is complete.
    #[serde(rename = "content_block_stop")]
    ContentBlockStop {
        /// Zero-based index of the content block.
        index: u32,
    },
    /// `message_delta` — incremental update to the message metadata.
    /// Emitted just before `message_stop` (or just after the last
    /// `content_block_stop`).
    #[serde(rename = "message_delta")]
    MessageDelta {
        /// Message-level metadata changes.
        delta: MessageDelta,
        /// Cumulative usage at this point in the stream.
        usage: MessageDeltaUsage,
    },
    /// `message_stop` — emitted once at the end of a stream.
    #[serde(rename = "message_stop")]
    MessageStop,
}

/// Delta payload inside a `content_block_delta` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentDelta {
    /// `text_delta` — incremental text.
    #[serde(rename = "text_delta")]
    TextDelta {
        /// The text fragment.
        text: String,
    },
    /// `input_json_delta` — partial JSON for a `tool_use` block's input.
    /// Concatenate across events for the same `index` to get the full
    /// input object.
    #[serde(rename = "input_json_delta")]
    InputJsonDelta {
        /// A partial JSON fragment (not necessarily valid JSON on its
        /// own — concatenate across events).
        partial_json: String,
    },
    /// `thinking_delta` — incremental thinking text.
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        /// The thinking text fragment.
        thinking: String,
    },
    /// `signature_delta` — thinking block signature.
    #[serde(rename = "signature_delta")]
    SignatureDelta {
        /// The signature fragment.
        signature: String,
    },
    /// `citations_delta` — citations for a text block. Sylvander v2 does
    /// not strong-type citations; the JSON is passed through.
    #[serde(rename = "citations_delta")]
    CitationsDelta {
        /// Opaque citations payload.
        citation: JsonValue,
    },
}

/// Message-level metadata delta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MessageDelta {
    /// Why the model stopped generating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// Which custom stop sequence was generated, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
}

/// Cumulative usage reported in `message_delta` events. Token counts
/// accumulate across the stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MessageDeltaUsage {
    /// Cumulative input tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    /// Cumulative output tokens.
    pub output_tokens: u32,
    /// Cumulative cache creation input tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// Cumulative cache read input tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn message_start_round_trip() {
        let json = json!({
            "type": "message_start",
            "message": {
                "id": "msg_abc",
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": "claude-sonnet-5-20260601",
                "stop_reason": null,
                "usage": {"input_tokens": 5, "output_tokens": 1}
            }
        });
        let event: RawStreamEvent = serde_json::from_value(json).unwrap();
        match event {
            RawStreamEvent::MessageStart { message } => {
                assert_eq!(message.id, "msg_abc");
            }
            other => panic!("expected MessageStart, got {other:?}"),
        }
    }

    #[test]
    fn content_block_delta_text() {
        let json = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hello"}
        });
        let event: RawStreamEvent = serde_json::from_value(json).unwrap();
        match event {
            RawStreamEvent::ContentBlockDelta { index, delta } => {
                assert_eq!(index, 0);
                match delta {
                    ContentDelta::TextDelta { text } => assert_eq!(text, "Hello"),
                    other => panic!("expected TextDelta, got {other:?}"),
                }
            }
            other => panic!("expected ContentBlockDelta, got {other:?}"),
        }
    }

    #[test]
    fn content_block_delta_input_json() {
        let json = json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "{\"loc"}
        });
        let event: RawStreamEvent = serde_json::from_value(json).unwrap();
        match event {
            RawStreamEvent::ContentBlockDelta { index, delta } => {
                assert_eq!(index, 1);
                match delta {
                    ContentDelta::InputJsonDelta { partial_json } => {
                        assert_eq!(partial_json, "{\"loc");
                    }
                    other => panic!("expected InputJsonDelta, got {other:?}"),
                }
            }
            other => panic!("expected ContentBlockDelta, got {other:?}"),
        }
    }

    #[test]
    fn ping_round_trip() {
        let json = json!({"type": "ping"});
        let event: RawStreamEvent = serde_json::from_value(json).unwrap();
        assert!(matches!(event, RawStreamEvent::Ping));
    }

    #[test]
    fn message_delta_round_trip() {
        let json = json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 50}
        });
        let event: RawStreamEvent = serde_json::from_value(json).unwrap();
        match event {
            RawStreamEvent::MessageDelta { delta, usage } => {
                assert_eq!(delta.stop_reason, Some(StopReason::EndTurn));
                assert_eq!(usage.output_tokens, 50);
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    #[test]
    fn message_stop_round_trip() {
        let json = json!({"type": "message_stop"});
        let event: RawStreamEvent = serde_json::from_value(json).unwrap();
        assert!(matches!(event, RawStreamEvent::MessageStop));
    }
}