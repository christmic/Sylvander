# `sylvander-benchmark-runtime`

This non-production crate owns the coordinate and result contracts for rich
Runtime scenarios. It does not own Runtime algorithms, external verifier truth,
provider protocols, or production fault injection.

The atomic coordinate is:

```text
suite × revision × scenario × topology × workspace × failure point
× cognition profile × exact model set × run
```

Local harnesses cover durable recovery, multi-Agent graph governance,
workspace concurrency, cognition ablations, multimodal perception, and Doctor
experiments. External adapters retain the official suite reward unchanged.
Runtime safety is separately defined as useful completion with no invariant
violation, duplicate effect, or user-visible failure.

Every repetition remains an individual record. Unsupported cells, harness
errors, verifier failures, and missing credentials must be explicit in the
future runner status contract; they are never removed from matrix coverage.

The checked-in CLI validates exact plan artifacts and deterministically reduces
result arrays without blending task reward into Runtime safety:

```sh
cargo run -p sylvander-benchmark-runtime --bin sylvander-runtime-bench -- \
  validate-plan path/to/plan.json
cargo run -p sylvander-benchmark-runtime --bin sylvander-runtime-bench -- \
  summarize path/to/results.json
```

Detailed capability boundaries and the selected primary-source benchmark
portfolio are documented in
[`sylvander-runtime/docs/agent-cognition-workflow-doctor.md`](../sylvander-runtime/docs/agent-cognition-workflow-doctor.md).
The version-pinning and adapter inventory is maintained in
[`sylvander-runtime/docs/runtime-benchmark-corpus.md`](../sylvander-runtime/docs/runtime-benchmark-corpus.md).

Verification:

```sh
cargo test -p sylvander-benchmark-runtime --locked
cargo clippy -p sylvander-benchmark-runtime --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p sylvander-benchmark-runtime --no-deps --locked
```
