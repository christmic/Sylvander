# Agent benchmark program

## Selection evidence

The benchmark set is capability-led rather than leaderboard-led. Sylvander is
currently a server-owned, tool-using coding and terminal Agent; therefore the
first adapters measure software engineering, terminal execution, and
policy-bound API use.

- [Harbor](https://github.com/harbor-framework/harbor) defines isolated tasks,
  verifier rewards, Agent adapters, and ATIF trajectory output. Sylvander pins
  implementation evidence to `ea2fee78517f2e591bad69fcf1e6731f9c23ec99`.
- [Terminal-Bench](https://github.com/harbor-framework/terminal-bench) measures
  end-to-end terminal tasks with task-specific tests and reference solutions.
  Dataset names and versions are always recorded; an unversioned score is
  invalid evidence.
- [SWE-bench](https://github.com/SWE-bench/SWE-bench) evaluates patches for
  real GitHub issues using its official Docker harness. Prediction export must
  retain `instance_id`, patch, and Agent/model identity, while normalized
  Sylvander evidence excludes patch contents.
- [τ³-bench](https://github.com/sierra-research/tau2-bench) is the current
  successor to the outdated original τ-bench tasks. Sylvander pins its current
  adapter evidence to `79975ac5741e23fbb1d2ac44262d62398a6d87bd`, including
  `src/tau2/agent/base_agent.py`, `base/participant.py`,
  `data_model/message.py`, and `orchestrator/orchestrator.py`.

AgencyBench and general browser/desktop suites remain tracked candidates, not
initial gates: their visual, browser, research, or very-long-horizon scenarios
need production capabilities and resource budgets that the first adapter does
not yet claim.

## Dimensions and aggregation

An individual result is never identified by model name alone:

```text
benchmark
  × dataset version
  × task
  × agent revision
  × provider
  × protocol
  × model
  × run ordinal
```

Task rewards come only from the benchmark's verifier. The benchmark may derive
aggregate pass rate, mean reward, latency, iterations, tool calls, token usage,
and failure taxonomy, but it must not replace or reinterpret the benchmark's
primary metric. Repeated runs are retained individually before aggregation.

The user-facing meaning of every status, score, portfolio member, and execution
profile is normative in `docs/agent-benchmark-scorecard.md`. In particular, a
harness exception is not a verifier reward of zero, and a one-task smoke run is
not an Agent capability baseline.

## Delivery order

1. ATIF v1.7 value contract and fail-closed event recorder;
2. Harbor Agent adapter plus a deterministic local task fixture;
3. versioned Terminal-Bench adapter smoke, stratified regression subset, then
   the full release baseline;
4. SWE-bench prediction export and official-harness smoke task;
5. τ³-bench half-duplex user/tool adapter;
6. explicit regression thresholds only after repeat variance is measured.

Every live gate records exact code, harness, dataset, environment, and model
coordinates. Missing infrastructure, credentials, verifier output, or terminal
trajectory evidence is `not_run`/`infrastructure_error`, never a skip-pass.
The runner also checkpoints the public `AgentEvent` stream throughout the run;
partial ATIF is retained after timeout or interruption and records request,
retry, tool, usage/cache, and terminal lifecycle evidence without raw
credentials. Verifier reward remains Harbor-owned and is never inferred from
these operational events.

## Known capability gap

τ³ half-duplex execution requires `generate_next_message` to return either user
text or a structured tool call; its orchestrator executes that domain tool and
passes a `ToolMessage` into the next call. Sylvander's current `AgentLoop`
authorizes and executes a registered tool internally before continuing its next
model iteration. It has no production port for suspending on an externally
owned tool call and resuming from an external result. Therefore τ³ cells are
`not_applicable_capability` for the current Agent revision. A direct-model
adapter would measure the provider rather than Sylvander and is forbidden.

Closing this gap requires an Agent/Runtime feature with focused module tests:
an externally executed tool boundary that preserves immutable turn authority,
call correlation, authorization, durable suspension, and restart-safe resume.
Only then may the benchmark add the thin τ³ `HalfDuplexAgent` bridge.
