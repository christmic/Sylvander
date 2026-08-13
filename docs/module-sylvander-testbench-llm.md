# `sylvander-testbench-llm`

## Ownership

`sylvander-testbench-llm` is a non-production, high-dimensional acceptance and
comparison module. Its atomic coordinate is:

```text
protocol × provider × model × scenario × run
```

It owns:

- declarative matrix validation and expansion;
- explicit capability applicability for every matrix cell;
- credential-gated live journey orchestration;
- controlled fault injection through public production boundaries;
- scoring, cross-coordinate comparison, and content-safe evidence output.

It does **not** own provider request/response types, protocol conversion,
stream parsing, production retry, credential discovery, or durable recovery.
It does not replace or relocate tests belonging to a production module.

## Test-layer boundary

Each `sylvander-llm-*` provider crate retains its own:

1. unit tests for request conversion, feature gates, usage normalization, and
   error classification;
2. deterministic integration tests using official-shaped HTTP/SSE fixtures;
3. narrowly scoped, ignored real-API tests proving that the adapter still
   speaks its declared protocol.

The testbench consumes those already-tested public adapters and answers a
different question: how a declared set of models and providers behaves across
the same scenario set. A passing testbench cell cannot compensate for a failed
provider test, and a passing provider test is not a cross-model benchmark.

Agent owns retry policy, while Runtime owns durable process recovery. The
testbench may compose those public boundaries to measure retry and recovery,
but the implementation and focused tests stay in their owning crates.

## Matrix semantics

Protocol and provider identities are independent. Multiple providers may
implement one protocol, and one provider may expose multiple protocols. Every
protocol binding enumerates multiple model deployments and declares protocol
scenario support; every model separately advertises applicable scenarios.

Expansion emits every selected scenario and repetition for every
`provider/protocol/model` coordinate. Unsupported cells are retained as
`not_applicable_protocol` or `not_applicable_model`; they are never silently
filtered or counted as passes. Duplicate provider/protocol/model coordinates
and empty dimensions are invalid input.

The runner separates review from execution. `plan` expands and emits only
content-safe coordinates without resolving credentials or dispatching network
requests. `run` is the explicit, potentially billable operation.

Repository-tracked templates live under `sylvander-testbench-llm/matrices`:

- `live.example.json` demonstrates multiple providers per protocol, multiple
  protocols per provider, multiple models per binding, and explicit model
  applicability;
- `minimax.live.json` and `aliyun-token-plan.live.json` are versioned live
  deployment matrices. They contain endpoint and credential-variable names,
  never credential values;
- `fault.example.json` targets disposable controlled endpoints. Production
  provider endpoints must never be used to provoke rate limits, timeouts, or
  server failures. Its bindings cover OpenAI Responses, OpenAI Chat
  Completions, and Anthropic Messages independently.

Review a completed matrix before executing it:

```sh
cargo run -p sylvander-testbench-llm --bin sylvander-llm-bench -- plan path/to/matrix.json
cargo run -p sylvander-testbench-llm --bin sylvander-llm-bench -- run path/to/matrix.json
```

`run` is serial and exits non-zero for `failed`, `not_run`, or
`infrastructure_error`. It never treats a missing credential as a skip-pass.

## Dependency boundary

The crate may depend on `sylvander-llm-core`, current provider adapters, and the
public Agent/Runtime boundaries required by a measured scenario. No production
crate may depend on `sylvander-testbench-llm`; dependency direction is always
production to testbench consumer, never the reverse.

Secrets enter only through an explicitly named environment-variable binding.
The matrix stores that variable's name, never its value. Credentials are never
represented by the result schema.

## Verification

```sh
cargo test -p sylvander-testbench-llm --locked
cargo clippy -p sylvander-testbench-llm --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p sylvander-testbench-llm --no-deps --locked
```

The complete case matrix and live configuration contract are documented in
[`llm-live-conformance.md`](llm-live-conformance.md).
