# `sylvander-llm-anthropic` architecture

This crate is the Anthropic implementation of the provider-neutral
`sylvander-llm-core::ModelProvider` contract. It translates between core
requests/events and the Anthropic Messages wire format; it does not make
session, retry, authorization, or UI decisions.

## Data path

```text
AgentRun
  -> ModelRequest (sylvander-llm-core)
  -> AnthropicProvider
  -> convert::{request_to_anthropic, event_to_core}
  -> AnthropicClient / Messages API / SSE
  -> ModelEventStream
  -> AgentRun
```

`api/` is a direct, typed SDK for the Messages endpoints. `provider.rs` adapts
that SDK into the core `ModelProvider` trait. `convert.rs` is the only
provider-specific translation seam and preserves opaque reasoning state for
subsequent turns without letting core inspect vendor payloads.

## Public boundary

- `AnthropicClient` and `MessagesApi` provide direct typed access to the
  Messages API, including SSE streaming and token counting.
- `AnthropicProvider` implements `ModelProvider` for Runtime composition.
- `prelude` contains the SDK types intended for direct consumers. New API
  types should be added deliberately rather than relying on broad glob exports.

The provider accepts a configured base URL for controlled proxies and test
servers. Credentials are supplied at construction and must never be rendered
in `Display`, protocol errors, tracing fields, or persisted evidence.

## Failure and streaming contract

The adapter maps vendor failures into `ProviderError` with a stable kind and
phase. It does not retry internally: the Agent loop owns retry policy so a
turn has one visible retry budget. SSE parsing emits only normalized
`TextDelta`, `ReasoningDelta`, and one terminal `Completed` event. A malformed
or prematurely terminated stream is a `ProviderError`, never a fabricated
completion.

Capability validation happens before dispatch in `sylvander-llm-core`. The
adapter must reject any additional vendor-only unsupported combination with a
content-safe error and must not silently remove requested tools, reasoning,
media, caching, or structured-output constraints.

## Verification

The crate's unit tests cover typed request construction, SSE parsing, response
assembly, errors, batching, and conversion. Wire-level tests use local mock
servers; real-provider tests remain explicitly ignored because they require
operator credentials.

Run the crate documentation and tests with:

```bash
cargo doc -p sylvander-llm-anthropic --no-deps --locked
cargo test -p sylvander-llm-anthropic --locked
```

## Related documentation

- [`../../docs/module-sylvander-llm-core.md`](../../docs/module-sylvander-llm-core.md)
  — the provider-neutral contract.
- [`../../docs/sylvander-agent-platform.md`](../../docs/sylvander-agent-platform.md)
  — model selection and Agent execution ownership.
- [`../README.md`](../README.md) — direct SDK usage examples.
