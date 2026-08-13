# `sylvander-testbench-llm`

## Ownership

`sylvander-testbench-llm` is the non-production acceptance module for every
Sylvander LLM adapter. It owns credential-gated live journeys, deterministic
fault injection, and the content-safe machine-readable result contract.

Provider crates continue to own official-derived wire fixtures and local
conversion tests. They never depend on the testbench. Agent owns retry policy,
while Runtime owns durable process recovery; the testbench may compose those
public boundaries without moving either responsibility into a provider.

## Dependency boundary

The crate may depend on `sylvander-llm-core` and all current provider adapters.
No production crate may depend on `sylvander-testbench-llm`. Secrets enter only
through explicitly selected ignored live tests and are never represented by the
result schema.

## Verification

```sh
cargo test -p sylvander-testbench-llm --locked
cargo clippy -p sylvander-testbench-llm --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p sylvander-testbench-llm --no-deps --locked
```

The complete case matrix and live configuration contract are documented in
[`llm-live-conformance.md`](llm-live-conformance.md).
