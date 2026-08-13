# LLM live conformance and reliability bench

This document defines the credential-gated acceptance suite for Sylvander's
LLM adapters. It supplements the deterministic protocol tests described in
[`llm-provider-protocols.md`](llm-provider-protocols.md); it never replaces
official-derived fixtures or makes ordinary workspace tests depend on an
external service.

## Scope and ownership

The suite is a high-dimensional bench whose atomic coordinate is `protocol ×
provider × model × scenario × run`. It verifies production paths from an
explicitly configured provider adapter through the provider-neutral
`ModelProvider` stream contract and compares equivalent scenario results across
coordinates.

Provider crates retain their unit, fixture integration, and narrow ignored
real-API tests. The benchmark neither moves nor replaces them. Runtime owns
configuration, credential resolution, deadlines, durable turn state, and
process recovery. Provider crates own request encoding, HTTP/SSE handling,
error classification, and exact usage conversion. Agent owns the single retry
budget for failures that occur before a stream opens. The benchmark owns only
matrix orchestration, applicability, scoring, comparison, and evidence.

This ownership creates two different recovery claims:

- a provider test may prove that a timeout, transport loss, or truncated stream
  becomes the correct typed error without fabricating a completion;
- a Runtime journey may prove that a killed process reopens durable state as
  interrupted and does not replay an uncertain side effect.

Provider code must not implement hidden retry or process persistence to make a
bench pass.

## Evidence baselines

Live behavior must remain consistent with the pinned official sources already
recorded in [`llm-provider-protocols.md`](llm-provider-protocols.md):

- Anthropic Python SDK `009b035305e0724ce108ebd796935f91711fc6e1`;
- OpenAI Python SDK `a1eeab58db02de46717ccebaf1eb83e314fa86ff`;
- DashScope Python SDK `397e02b02596e29b03d3ec7159c3610d6dac65e6`.

Each recorded run also pins the Sylvander commit, provider protocol kind,
provider ID, model ID, endpoint origin, case revision, and execution time.
Credentials, request bodies, response text, and provider error bodies are not
result fields.

## Case matrix

| Case | Real endpoint | Controlled fault endpoint | Acceptance |
| --- | --- | --- | --- |
| connectivity | required | no | stream opens, emits exactly one completion, and returns non-empty text |
| usage | required | no | input and output usage are positive and all reported optional dimensions are preserved |
| image input | required when selected by the provider protocol binding | no | a fixed inline PNG is decoded and its concealed digit is identified exactly |
| token count | required when the protocol exposes a count operation | no | remote count is positive and recorded separately from generated-response usage |
| cache write/read | required for an advertised cache-capable model | no | an explicit cache breakpoint reports positive creation tokens on the first call; an implicit-cache protocol establishes a fresh prefix; a bounded repeated call reports positive cache-read tokens |
| open timeout | no | required | the configured deadline yields `Timeout` in the `Open` phase |
| transient retry | no | required | retryable open failures consume the exact Agent retry budget and a later success completes once |
| truncated stream | no | required | visible deltas are never replayed and the stream terminates with a typed protocol/transport failure |
| process interruption | required behind a delaying proxy | required | Runtime restart records interruption, never false completion, and never replays an uncertain tool effect |

Image understanding and image generation are distinct capabilities. A text
chat binding that does not accept image blocks records `image_input` as a
protocol-level N/A. Provider-specific image generation endpoints require
separate `text_to_image` and `image_to_image` cases; a successful generation
must not be reported as chat image understanding.

Unsupported provider capabilities are explicit `not_applicable` results, not
passes. Missing configuration is `not_run` and makes an explicitly requested
live gate fail. Ordinary `cargo test` does not select credential-gated cases.

## Provider matrix

The initial live matrix covers:

| Protocol | Connectivity and usage | Remote token count | Cache assertion |
| --- | --- | --- | --- |
| Anthropic Messages | required | required through `messages/count_tokens` | explicit ephemeral breakpoint |
| OpenAI Responses | required | not claimed until an implemented official operation exists | repeated stable prefix and reported cached input |
| OpenAI Chat Completions | required | not claimed until an implemented official operation exists | repeated stable prefix and reported cached prompt |
| DashScope Generation | required | not claimed by the current native Generation adapter | repeated stable prefix where the selected model advertises context cache |

The selected live model must advertise every capability used by a case. A test
must not infer cache, reasoning, tool, image, document, or structured-output
support from a model name.

## Configuration and secret handling

The live runner reads only bench-scoped environment variables. Production LLM
crates continue to receive endpoint and credential values as constructor
arguments and never discover them from the process environment.

Required per selected provider use an explicit provider prefix. The initial
Anthropic variables are:

- `SYLVANDER_BENCH_ANTHROPIC_BASE_URL`;
- `SYLVANDER_BENCH_ANTHROPIC_API_KEY`;
- `SYLVANDER_BENCH_ANTHROPIC_MODEL`.

OpenAI and DashScope use the same suffixes with `OPENAI` or `DASHSCOPE` in
place of `ANTHROPIC`; protocol-specific model variables may narrow one shared
provider configuration when its supported wire contracts require different
models.

OpenAI uses `SYLVANDER_BENCH_OPENAI_RESPONSES_MODEL` and
`SYLVANDER_BENCH_OPENAI_CHAT_MODEL` with the shared OpenAI endpoint and key.

The runner may also accept provider/protocol selection, request timeout, retry
budget, maximum output tokens, and maximum billed input tokens. Defaults must
be conservative. Debug output and serialized results must never contain the
credential, authorization headers, request body, response text, or raw
provider error body.

## Result contract

Every case emits one JSON object with these stable dimensions:

```text
schema_version
run_id
case_id + case_revision
scenario + run_ordinal
status = passed | failed | not_run | not_applicable | infrastructure_error
sylvander_commit + worktree_dirty
provider_id + protocol + model_id + endpoint_origin
started_at + duration_ms + attempts
input_tokens + output_tokens
cache_write_tokens + cache_read_tokens
reasoning_tokens + reported_total_tokens
failure_kind + failure_phase
```

Unknown token dimensions remain absent rather than becoming zero. The endpoint
field contains only scheme, host, and explicit port. Human-readable failure
text is a fixed Sylvander-owned diagnostic, never provider content.

## Reproducibility and gates

The suite has three execution tiers:

1. Deterministic pull-request tests use local official-shaped HTTP/SSE fixtures
   and controlled time, including timeout, retry, and truncated-stream cases.
2. Scheduled live smoke runs one low-output connectivity/usage case for every
   configured protocol and fails on `not_run`.
3. Release acceptance adds remote token count, cache creation/read, and the
   Runtime process-interruption journey for every advertised deployment.

Results from different model IDs, protocol kinds, case revisions, dataset
revisions, or Sylvander commits are not merged into one baseline. Infrastructure
errors are reported separately from adapter failures. A release claim requires
zero failed required cases and zero unwaived infrastructure errors.

## Cost and safety bounds

- Live cases run serially unless an operator explicitly approves concurrency.
- Connectivity uses the smallest practical output limit.
- Cache cases declare their maximum input size before dispatch and make at most
  two billed calls per attempt.
- Retry faults are injected locally; the suite does not provoke rate limits or
  server errors against a provider's production endpoint.
- Process-interruption testing uses a dedicated delayed proxy and a disposable
  Runtime data directory.
- Every temporary artifact has an explicit cleanup path; failed durable state
  is retained only when the operator requests diagnostic preservation.

## Completion rule

An adapter is live-conformant only when its deterministic protocol suite and
all applicable required live cases pass against the same tracked Sylvander
commit. A skipped, missing-key, stale-result, compatible-provider-only, or
fixture-only run is never evidence of live conformance.
