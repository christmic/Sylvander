# `sylvander-llm-dashscope` architecture

This crate implements the native
`/api/v1/services/aigc/text-generation/generation` protocol. It does not route
through the OpenAI-compatible endpoint.

## Data path

```text
ModelRequest
  -> DashScopeProvider
  -> convert.rs
  -> api/{client,types,sse,stream,error}.rs
  -> native Generation SSE
```

The typed API layer preserves `event:`, `status:`, and `data:` SSE fields.
`event:error` becomes an API failure with status and request ID. Successful
chunks assemble text, reasoning content, indexed function-call fragments,
finish reason, cached prompt tokens, reasoning tokens, and total usage.
Completion requires both a finish reason and usage; premature EOF fails closed.

## Capability boundary

Native Generation supports text conversation, function tools and tool results,
thinking enablement and budget, compatible reasoning replay, parallel tool
calls, and the complete usage dimensions reported by the pinned SDK.

The following are intentionally not advertised as Generation capabilities:

- Image/document content belongs to DashScope MultiModalConversation rather
  than the native Generation request shape.
- `response_format={"type":"json_object"}` requests JSON syntax but cannot
  enforce the caller's JSON Schema, so it does not satisfy core's
  `structured_output` capability.
- Qualitative reasoning effort has no native Generation field. A concrete
  thinking budget is supported; effort-only requests fail closed.

These are protocol distinctions, not silent feature omissions. Adding a new
DashScope API requires its own typed API module and Registry protocol kind.

## Evidence and tests

The pinned evidence baseline is dashscope-sdk-python
`397e02b02596e29b03d3ec7159c3610d6dac65e6`. Tests reference
`generation.py`, `common/utils.py`, `dashscope_response.py`, and the official
Generation samples. They cover normal, error, truncation, reasoning, tool-call,
and token-usage paths without live credentials.
