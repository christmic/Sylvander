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
  successor to the outdated original τ-bench tasks. Its half-duplex Agent
  contract accepts user/tool messages and returns assistant messages, which
  maps to a future Runtime interaction adapter rather than the terminal runner.

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

Task rewards come only from the benchmark's verifier. The testbench may derive
aggregate pass rate, mean reward, latency, iterations, tool calls, token usage,
and failure taxonomy, but it must not replace or reinterpret the benchmark's
primary metric. Repeated runs are retained individually before aggregation.

## Delivery order

1. ATIF v1.7 value contract and fail-closed event recorder;
2. Harbor Agent adapter plus a deterministic local task fixture;
3. versioned Terminal-Bench smoke subset, then representative baseline;
4. SWE-bench prediction export and official-harness smoke task;
5. τ³-bench half-duplex user/tool adapter;
6. explicit regression thresholds only after repeat variance is measured.

Every live gate records exact code, harness, dataset, environment, and model
coordinates. Missing infrastructure, credentials, verifier output, or terminal
trajectory evidence is `not_run`/`infrastructure_error`, never a skip-pass.
