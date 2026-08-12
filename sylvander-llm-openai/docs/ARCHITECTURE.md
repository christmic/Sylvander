# `sylvander-llm-openai` architecture

This crate implements two distinct wire protocols. They share only transport,
SSE framing, scalar conversion, and normalized error mapping; they do not share
request or stream state machines.

## Data paths

```text
ModelRequest
  -> OpenAiProvider
  -> convert/responses.rs
  -> api/responses/{types,stream}.rs
  -> OpenAI Responses SSE

ModelRequest
  -> OpenAiProvider
  -> convert/chat.rs
  -> api/chat/{types,stream}.rs
  -> Chat Completions SSE
```

`api/client.rs` owns authenticated HTTP dispatch. `api/error.rs` parses HTTP
status, provider request ID, and retry delay without retaining provider error
messages. `api/sse.rs` owns byte framing across arbitrary chunks, CRLF/LF,
comments, and multi-line data fields.

## Responses contract

The neutral adapter supports ordered text, image, and file input; function
tools and outputs; reasoning effort and opaque reasoning replay; strict JSON
Schema output; text and reasoning deltas; completed and incomplete terminal
events; refusal; and all reported token details.

Every event in the pinned official `ResponseStreamEvent` union is classified.
Lifecycle events that carry no neutral delta are explicitly ignored. Unknown
events fail closed. A final output item that belongs to an unconfigured built-in
tool also fails closed rather than being silently dropped.

OpenAI built-in tools, background responses, hosted conversations, reusable
prompts, and audio are separate product capabilities. Runtime does not
advertise them and core has no neutral representation for them. Adding one
requires a typed request/output model, Registry capability, conversion, and
official-derived fixture together.

## Chat Completions contract

The neutral adapter supports system/user/assistant/tool messages, ordered text
and image user parts, function tools, compatible-provider reasoning content,
reasoning effort, strict JSON Schema output, refusal, tool-call fragments, and
all official usage detail fields.

Every streaming request sends `stream_options.include_usage=true`. Completion
requires the official `[DONE]` marker, a finish reason, and the requested usage
tail chunk. EOF, malformed tool arguments, additional unrequested choices, and
missing usage are protocol failures, never fabricated completions.

## Evidence and tests

The pinned evidence baseline is openai-python
`a1eeab58db02de46717ccebaf1eb83e314fa86ff`. Contract tests name the reviewed
SDK path and cover normal, exceptional, and boundary behavior without live
credentials.
