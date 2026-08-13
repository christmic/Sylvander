# `sylvander-testbench-agent`

## Ownership

`sylvander-testbench-agent` is Sylvander's non-production Agent evaluation
module. Its atomic comparison coordinate is:

```text
benchmark × dataset version × task × agent revision × provider × protocol × model × run
```

It owns external-harness adapters, task/run matrix expansion, trajectory
export, verifier-result ingestion, normalized scoring, and regression evidence.
It does not own Agent policy, Runtime Session admission, provider protocols,
tool implementations, execution sandboxes, or benchmark task truth.

No production crate may depend on this crate. The testbench consumes public
Agent/Runtime/provider boundaries and external benchmark contracts; dependency
direction never points from production into evaluation code.

## Test-layer boundary

Agent unit tests prove local state-machine invariants such as retry count,
event order, tool authorization, iteration limits, and terminal outcomes.
Agent/Runtime integration tests prove composed execution, durable Session
commit, interruption, recovery, and concrete execution adapters.

The Agent testbench asks a different question: whether an exact Sylvander
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

## External benchmark adapters

Harbor is the primary execution harness because its task contract separates
instruction, isolated environment, verifier, reward, and trajectory, and it
already hosts Terminal-Bench-style datasets. A Sylvander Harbor adapter owns
only setup and invocation inside the harness-selected environment; Harbor owns
task lifecycle and verifier execution.

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

## Evidence and secrets

Every normalized result must include the Sylvander commit and dirty state,
benchmark and immutable dataset version, task identifier, Agent revision,
provider/protocol/model coordinate, run ordinal, verifier reward, duration,
iteration/tool counts, and token usage. Raw prompts, tool output, reasoning,
credentials, and benchmark secrets do not enter aggregate result records.
Detailed trajectories remain separate artifacts with harness-controlled access.

Credentials enter only through explicitly named environment variables during
an explicit live run. Dataset downloads, container pulls, verifier commands,
and billable model calls are never triggered by `cargo test`.

## Verification

```sh
cargo test -p sylvander-testbench-agent --locked
cargo clippy -p sylvander-testbench-agent --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p sylvander-testbench-agent --no-deps --locked
```

External live runs additionally validate exported trajectories with Harbor's
reference validator and retain the exact harness/dataset revision.
