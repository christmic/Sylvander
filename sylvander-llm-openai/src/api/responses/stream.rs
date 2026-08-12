//! `OpenAI` Responses SSE decoding with explicit terminal semantics.

use futures_util::StreamExt as _;
use reqwest::Response as HttpResponse;
use serde_json::Value;

use crate::api::OpenAiError;
use crate::api::responses::Response;
use crate::api::sse::SseParser;

#[derive(Debug, Clone, PartialEq)]
pub enum ResponseStreamEvent {
    OutputTextDelta(String),
    ReasoningDelta(String),
    Completed(Response),
    Incomplete(Response),
}

pub struct ResponsesStream {
    inner: PinStream,
}

type PinStream = std::pin::Pin<
    Box<dyn futures_util::Stream<Item = Result<ResponseStreamEvent, OpenAiError>> + Send>,
>;

impl ResponsesStream {
    pub(crate) fn new(response: HttpResponse) -> Self {
        let stream = async_stream::try_stream! {
            let mut body = response.bytes_stream();
            let mut parser = SseParser::default();
            let mut terminal = false;
            while let Some(chunk) = body.next().await {
                for event in parser.feed(&chunk?) {
                    let event = event?;
                    if event.data == "[DONE]" {
                        continue;
                    }
                    if terminal {
                        Err(OpenAiError::Protocol(
                            "Responses event followed terminal response".into(),
                        ))?;
                    }
                    let value: Value = serde_json::from_str(&event.data)?;
                    if let (Some(sse_type), Some(data_type)) = (
                        event.event.as_deref(),
                        value.get("type").and_then(Value::as_str),
                    ) && sse_type != data_type
                    {
                        Err(OpenAiError::Protocol(
                            "Responses SSE event type disagrees with data".into(),
                        ))?;
                    }
                    if let Some(decoded) = decode(value)? {
                        terminal = matches!(
                            decoded,
                            ResponseStreamEvent::Completed(_) | ResponseStreamEvent::Incomplete(_)
                        ) || terminal;
                        yield decoded;
                    }
                }
            }
            parser.finish()?;
            if !terminal {
                Err(OpenAiError::Protocol("Responses stream ended before a terminal event".into()))?;
            }
        };
        Self {
            inner: Box::pin(stream),
        }
    }
}

impl futures_util::Stream for ResponsesStream {
    type Item = Result<ResponseStreamEvent, OpenAiError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}

fn decode(value: Value) -> Result<Option<ResponseStreamEvent>, OpenAiError> {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| OpenAiError::Protocol("Responses event has no type".into()))?;
    match kind {
        "response.output_text.delta" => {
            Ok(Some(ResponseStreamEvent::OutputTextDelta(delta(&value)?)))
        }
        "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
            Ok(Some(ResponseStreamEvent::ReasoningDelta(delta(&value)?)))
        }
        "response.completed" => Ok(Some(ResponseStreamEvent::Completed(response(&value)?))),
        "response.incomplete" => Ok(Some(ResponseStreamEvent::Incomplete(response(&value)?))),
        "error" => Err(OpenAiError::Protocol(
            "Responses stream emitted an error event".into(),
        )),
        "response.failed" => Err(OpenAiError::Protocol(
            "Responses stream emitted a failed response".into(),
        )),
        known if known_non_terminal(known) => Ok(None),
        unknown => Err(OpenAiError::Protocol(format!(
            "unknown Responses event type: {unknown}"
        ))),
    }
}

fn delta(value: &Value) -> Result<String, OpenAiError> {
    value
        .get("delta")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| OpenAiError::Protocol("Responses delta event has no delta".into()))
}

fn response(value: &Value) -> Result<Response, OpenAiError> {
    let response = value
        .get("response")
        .ok_or_else(|| OpenAiError::Protocol("terminal event has no response".into()))?;
    Ok(serde_json::from_value(response.clone())?)
}

fn known_non_terminal(kind: &str) -> bool {
    matches!(
        kind,
        "response.created"
            | "response.queued"
            | "response.in_progress"
            | "response.output_item.added"
            | "response.output_item.done"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.refusal.delta"
            | "response.refusal.done"
            | "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.done"
            | "response.image_generation_call.in_progress"
            | "response.image_generation_call.generating"
            | "response.image_generation_call.partial_image"
            | "response.image_generation_call.completed"
            | "response.file_search_call.in_progress"
            | "response.file_search_call.searching"
            | "response.file_search_call.completed"
            | "response.web_search_call.in_progress"
            | "response.web_search_call.searching"
            | "response.web_search_call.completed"
            | "response.code_interpreter_call.in_progress"
            | "response.code_interpreter_call.interpreting"
            | "response.code_interpreter_call.completed"
            | "response.code_interpreter_call_code.delta"
            | "response.code_interpreter_call_code.done"
            | "response.output_text.annotation.added"
            | "response.custom_tool_call_input.delta"
            | "response.custom_tool_call_input.done"
            | "response.mcp_call.in_progress"
            | "response.mcp_call_arguments.delta"
            | "response.mcp_call_arguments.done"
            | "response.mcp_call.completed"
            | "response.mcp_call.failed"
            | "response.mcp_list_tools.in_progress"
            | "response.mcp_list_tools.completed"
            | "response.mcp_list_tools.failed"
            | "response.audio.delta"
            | "response.audio.done"
            | "response.audio.transcript.delta"
            | "response.audio.transcript.done"
    )
}
