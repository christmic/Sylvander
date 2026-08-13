# `sylvander-benchmark-agent`

## Ownership

`sylvander-benchmark-agent` is Sylvander's non-production Agent evaluation
module. Its atomic comparison coordinate is:

```text
benchmark × dataset version × task × agent revision × provider × protocol × model × run
```

It owns external-harness adapters, task/run matrix expansion, trajectory
export, verifier-result ingestion, normalized scoring, and regression evidence.
It does not own Agent policy, Runtime Session admission, provider protocols,
tool implementations, execution sandboxes, or benchmark task truth.

No production crate may depend on this crate. The benchmark consumes public
Agent/Runtime/provider boundaries and external benchmark contracts; dependency
direction never points from production into evaluation code.

## Test-layer boundary

Agent unit tests prove local state-machine invariants such as retry count,
event order, tool authorization, iteration limits, and terminal outcomes.
Agent/Runtime integration tests prove composed execution, durable Session
commit, interruption, recovery, and concrete execution adapters.

The Agent benchmark asks a different question: whether an exact Sylvander
revision can complete independently verified tasks under a named external
harness and dataset version, with comparable cost, latency, steps, and
trajectory evidence. A benchmark pass cannot compensate for a failed module
test; a module test is not evidence of benchmark task performance.

## Interchange contract

The first interoperability boundary is Harbor's Agent Trajectory Interchange
Format, ATIF v1.7. The implementation is derived from Harbor commit
`ea2fee78517f2e591bad69fcf1e6731f9c23ec99`:

- `src/harbor/models/trajectories/trajectory.py`;
- `step.py`, `agent.py`, `tool_call.py`;
- `observation.py`, `observation_result.py`;
- `metrics.py`, `final_metrics.py`;
- `rfcs/0001-trajectory-format.md`.

`TrajectoryRecorder` emits one ATIF agent step per Sylvander model iteration.
Tool starts become structured calls, terminal tool events become correlated
observations, provider usage becomes per-step metrics, and `AgentOutcome`
becomes final metrics. Missing terminals, invalid event order, non-object tool
arguments, and dangling observation references fail closed.

## Observability boundary

`AgentEvent` is the single source of truth for live Agent activity. Production
Runtime consumes it for Session/runtime records; the benchmark independently
projects the same public stream into benchmark evidence. The benchmark must not
reimplement Agent state transitions or create a second event model.

The runner atomically replaces `/logs/agent/trajectory.json` at iteration and
tool lifecycle boundaries. Every checkpoint is valid ATIF, including a partial
active iteration, so a Harbor timeout, executor cancellation, or runner crash
leaves the previous complete JSON document available. High-volume text,
reasoning, and tool-output deltas are accumulated in memory and checkpointed at
the next lifecycle boundary rather than causing per-token filesystem writes.

`extra.sylvander_observability` contains an ordered, timestamped event ledger
with retry cause/delay, tool start/timeout/finish, response IDs, per-request
token/cache usage, compression, interaction, plan, and terminal state. Its
provider coordinate contains provider, protocol, model, base URL, and a short
SHA-256 credential fingerprint for correlating an authorized live run. Raw
credentials are never serialized. Prompts, reasoning, tool arguments, and tool
results remain only in the access-controlled detailed trajectory; normalized
and aggregate records remain content-safe.

Harbor `result.json` ingestion follows the pinned `TrialResult`,
`VerifierResult`, and `AgentContext` contracts. It cross-checks task/model
coordinates and token totals against ATIF before emitting normalized evidence;
missing verifier output is an infrastructure failure, never a passing skip.

## External benchmark adapters

Harbor is the primary execution harness because its task contract separates
instruction, isolated environment, verifier, reward, and trajectory, and it
already hosts Terminal-Bench-style datasets. A Sylvander Harbor adapter owns
only setup and invocation inside the harness-selected environment; Harbor owns
task lifecycle and verifier execution.

For local evidence, Harbor's Docker-compatible environment contract is backed
by rootless Podman. The benchmark-owned compatibility entrypoint forwards the
contract to Podman and translates only Compose's `--project-directory` into a
working-directory change required by podman-compose. It does not alter task
images, verifier commands, rewards, or Agent output. LiteLLM is outside this
path: the custom Agent invokes Sylvander's production provider adapters.

The initial benchmark families are:

1. Harbor/Terminal-Bench for end-to-end terminal work in reproducible task
   environments;
2. SWE-bench-compatible patch export and official Docker verification for
   real repository issues;
3. τ³-bench half-duplex integration for policy-bound tool/user interaction,
   after the production Agent supports externally executed tool suspension and
   resume.

The first executable slice targets Harbor terminal tasks because Sylvander
already owns coding tools and a Runtime-selected workspace executor. SWE-bench
adds patch packaging after the terminal path is stable. τ³-bench requires an
interactive user/tool bridge and follows separately. Browser, desktop, voice,
and multimodal benchmarks are not claimed until Sylvander exposes the matching
production capability.

The portfolio and score semantics are documented in
`docs/agent-benchmark-scorecard.md`. This module must never present an exception
as a zero verifier score or present an adapter smoke as a general Agent score.
Capability, reliability, efficiency, coverage, and repeated-run stability are
separate scorecard sections; unrelated suite rewards are not averaged into a
synthetic universal score.

## Evidence and secrets

Every normalized result must include the Sylvander commit and dirty state,
benchmark and immutable dataset version, task identifier, Agent revision,
provider/protocol/model coordinate, run ordinal, verifier reward, duration,
iteration/tool counts, and token usage. Raw prompts, tool output, reasoning,
credentials, and benchmark secrets do not enter aggregate result records.
Detailed trajectories remain separate artifacts with harness-controlled access.

Credentials enter only through explicitly named environment variables during
an explicit live run. The detailed trajectory retains only a one-way truncated
fingerprint, never the credential value. Dataset downloads, container pulls,
verifier commands, and billable model calls are never triggered by `cargo
test`.

The Harbor runner constructs the selected production adapter for Anthropic
Messages, OpenAI Responses, OpenAI Chat Completions, or native DashScope text
generation. Endpoint and compatibility features belong to the provider and
protocol binding; model identifiers never select wire behavior.

## Verification

```sh
cargo test -p sylvander-benchmark-agent --locked
cargo clippy -p sylvander-benchmark-agent --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p sylvander-benchmark-agent --no-deps --locked
```

External live runs additionally validate exported trajectories with Harbor's
reference validator and retain the exact harness/dataset revision.

On arm64 hosts, execution selects a semantically equivalent `linux/arm64`
benchmark image first. A benchmark-pinned image is immutable evidence, however:
if upstream publishes only `linux/amd64`, the run uses emulation and records the
host/image architecture pair instead of rebuilding the verifier and losing
score comparability.

The Podman/Harbor installation gate is separate from a scored benchmark. The
effective pinned Terminal-Bench 2.0 gate uses an architecture-matched static
musl runner and executes its self-check after upload. Environment startup,
runner execution, and cleanup completed with zero exceptions; the superseded
file-presence-only gate is not accepted as executable evidence. Install-only
makes no model call and is not recorded as task-performance evidence.

After Harbor writes its per-trial result and ATIF trajectory, normalize the
pair against one planned coordinate:

```sh
cargo run -p sylvander-benchmark-agent --bin sylvander-agent-bench -- \
  ingest coordinate.json result.json trajectory.json harbor-ea2fee78517
```

The command emits one content-safe JSON result and exits non-zero for a failed
verifier or infrastructure outcome.

Normalized records aggregate only within an exact benchmark, dataset version,
Agent revision, provider, protocol, and model coordinate. Aggregation retains
executed, failed, infrastructure, not-run, and not-applicable counts separately;
it never turns missing runs into successful samples.

```sh
cargo run -p sylvander-benchmark-agent --bin sylvander-agent-bench -- \
  aggregate results.jsonl
```
