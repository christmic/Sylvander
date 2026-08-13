# LLM Provider Protocol Evidence and Compatibility

This document is the implementation contract for Sylvander's LLM adapters. A
wire field, event, or provider-specific behavior must be supported by the
locally checked-out official SDK source or by the provider's official API
documentation. Compatibility must not be inferred from third-party clients.

## Reproducible official baselines

The following sibling repositories are the evidence snapshots used for this
work. The commit is part of the compatibility contract; updating an adapter
requires recording and reviewing a newer official commit.

| Protocol owner | Local checkout | Commit | Release ancestry |
| --- | --- | --- | --- |
| Anthropic | `../anthropic-sdk-python` | `009b035305e0724ce108ebd796935f91711fc6e1` | after `v0.121.0` |
| OpenAI | `../openai-python` | `a1eeab58db02de46717ccebaf1eb83e314fa86ff` | after `v3.0.0` |
| Alibaba Cloud DashScope | `../dashscope-sdk-python` | `397e02b02596e29b03d3ec7159c3610d6dac65e6` | after `v1.26.6` |

Primary source and test paths reviewed for each protocol:

- Anthropic Messages: `src/anthropic/resources/messages/messages.py`,
  `src/anthropic/types/`, `src/anthropic/lib/streaming/`, and
  `tests/api_resources/test_messages.py` in the Anthropic checkout.
- OpenAI Responses: `src/openai/resources/responses/responses.py`,
  `src/openai/types/responses/`, and `tests/api_resources/test_responses.py` in
  the OpenAI checkout.
- OpenAI Chat Completions:
  `src/openai/resources/chat/completions/completions.py`,
  `src/openai/types/chat/`, and
  `tests/api_resources/chat/test_completions.py` in the OpenAI checkout.
- DashScope Generation: `dashscope/aigc/generation.py`,
  `dashscope/api_entities/dashscope_response.py`, `tests/unit/test_messages.py`,
  and `tests/unit/test_http_api.py` in the DashScope checkout.

## Protocol identity and provider identity

A protocol kind identifies a wire contract. A provider ID identifies an
independently configured service using that contract. They are not aliases.
The supported protocol kinds are:

- `anthropic_compatible` (legacy spelling retained for stored configuration)
- `anthropic_messages`
- `openai_responses`
- `openai_chat_completions`
- `dashscope_generation`

For example, `openai`, `qwen-openai`, and `deepseek` may be different provider
IDs using `openai_chat_completions`, while `qwen-dashscope` uses
`dashscope_generation`. A provider definition always supplies its base URL and
credential binding explicitly. LLM adapter constructors always receive the
resolved API key and base URL as arguments and never discover credentials or
endpoints from environment variables.

## Feature contract

Model capabilities describe what a selected model can do. Provider features
describe optional wire behavior implemented by a particular endpoint. A
provider definition stores a validated set of feature names and the runtime
passes that set into its protocol adapter. Unknown features are rejected.

Initial feature vocabulary:

| Protocol | Feature switches |
| --- | --- |
| Anthropic Messages | none; typed core capabilities and reasoning settings determine the official request shape |
| OpenAI Responses | `enable_thinking` |
| OpenAI Chat Completions | `enable_thinking`, `max_completion_tokens`, `reasoning_content` |
| DashScope Generation | `enable_thinking`, `thinking_budget`, `parallel_tool_calls`, `reasoning_content` |

A feature switch only permits its wire behavior; request conversion must still
check the selected model's declared capabilities. Protocol-specific typed
feature structures are constructed from the persisted names before any HTTP
request is made. Arbitrary provider JSON is not accepted as a feature switch.

The runtime routes exclusively from `ProviderDefinition.kind`; it does not
guess a protocol from a provider name, model name, or URL. `sylvander-agent`
receives only the provider-neutral `ModelProvider` contract and its normal
dependency graph contains only `sylvander-llm-core` among LLM crates.

## Audio input boundary

Provider-neutral audio is an explicit `AudioContent` block containing base64
bytes and the closed `wav | mp3` format vocabulary. `audio_input` is independent
from vision, document input, tools, and reasoning; Runtime validates attachment
kind, MIME type, base64 encoding, non-empty bytes, and exact declared size
before a model request can be opened.

Only `openai_chat_completions` currently implements this capability. Its wire
shape is derived from pinned OpenAI SDK type
`src/openai/types/chat/chat_completion_content_part_input_audio_param.py`:
`{"type":"input_audio","input_audio":{"data":"...","format":"wav|mp3"}}`.
The current official model documentation also identifies the GPT Audio family
as accepting audio through Chat Completions:
<https://developers.openai.com/api/docs/models/gpt-audio> (verified
2026-08-14).

OpenAI Responses remains fail-closed. Although the pinned SDK exports a
`ResponseInputAudioParam` type, it is absent from both
`ResponseInputMessageContentListParam` and `ResponseInputItemParam`; therefore
the current request union does not establish a dispatchable audio input path.
Anthropic Messages and DashScope Generation likewise reject `audio_input`
during registry preflight. A type exported by an SDK, a permissive JSON field,
or a configured specialist name is not sufficient evidence of protocol
support.

## Usage preservation

`TokenUsage` keeps normalized input, output, cache-write, and cache-read counts.
Typed details preserve reported protocol dimensions such as Anthropic cache
TTL buckets and thinking tokens, and OpenAI audio, reasoning, and prediction
tokens. Native DashScope Generation preserves its reported cached prompt and
reasoning token details. An absent optional field means the provider omitted it
and is distinct from a reported zero. Adapter tests must assert both totals and
details.

All HTTP adapters enforce a bounded two-minute default request deadline and
offer an explicit constructor deadline for Runtime and conformance tests.
Deadline failures normalize to a retryable `Timeout` during the exact open or
stream phase; provider adapters do not retry internally.
HTTP 402 billing or exhausted-balance responses normalize to the non-retryable
`QuotaExceeded` kind and remain distinct from malformed requests.

OpenAI Chat Completions always requests the official streaming usage tail with
`stream_options.include_usage=true`; a completed stream without that requested
usage fails closed. OpenAI Responses treats both `response.completed` and
`response.incomplete` as terminal response events. Native DashScope preserves
SSE `event:error` status and request identity instead of interpreting an error
payload as empty output.

DashScope Generation does not advertise `vision`, `document_input`, or
`structured_output`: those capabilities require a different native API or a
stronger schema contract than Generation provides. They must not be inferred
from a permissive SDK dictionary parameter.

Provider-specific non-token usage metadata, including Anthropic service tier,
inference geography, and server-tool request counts, remains typed in the
Anthropic wire response. It must not be silently converted into token counts.

## Test provenance and model matrix

Each adapter has two test layers:

1. Official-derived contract tests reproduce request and response examples or
   event sequences from the pinned official SDK tests. Test names and comments
   identify the source SDK path and commit. They run against local mock HTTP/SSE
   servers and require no credentials.
2. Sylvander-owned regression tests cover neutral conversion, feature gating,
   malformed streams, preserved token dimensions, and provider/model routing.

Credential-gated connectivity, usage, cache, timeout, and recovery acceptance
is specified separately in
[`llm-live-conformance.md`](llm-live-conformance.md). A live run supplements but
never replaces either deterministic layer above.

The routing and serialization matrix must cover distinct model families rather
than testing one model string repeatedly:

| Protocol | Required model profiles |
| --- | --- |
| Anthropic Messages | current Claude model, Claude model with extended/adaptive thinking |
| OpenAI Responses | current GPT model, current reasoning model |
| OpenAI Chat Completions | OpenAI GPT model, GPT Audio input model, Qwen OpenAI-compatible model, DeepSeek OpenAI-compatible model |
| DashScope Generation | Qwen non-thinking model, Qwen thinking model, Qwen tool-calling model |

These are wire-level model profiles, not live conformance claims. A model is
listed as supported only when its request shape is validated by the official
provider documentation or SDK and represented by a passing fixture. Optional
credential-gated live tests may supplement this matrix but are never the sole
evidence.

## Rust source layout

All imports in new or modified Rust modules belong in the module-level import
section. Function- or block-local `use` declarations are forbidden unless a
compiler, macro, or conditional-compilation constraint makes them unavoidable;
such an exception requires an adjacent `// Local import required: ...` comment.
CI scans modified Rust files for undocumented local imports.
