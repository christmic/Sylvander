# Agent benchmark scorecard

## What one benchmark result means

An Agent benchmark result is evidence for one exact coordinate, not a property
of a model name alone:

```text
benchmark × dataset version × task × Agent revision
  × provider × protocol × model × run ordinal
```

The task-owned verifier decides the primary reward. Sylvander records the
reward without replacing it with an LLM judge or a home-grown interpretation.
For binary task suites, reward `1` means the task verifier accepted the final
environment state and reward `0` means it did not. A fractional reward is
retained only when the upstream benchmark defines one.

These outcomes are deliberately distinct:

| Outcome | Verifier ran? | Included in capability score? | Meaning |
| --- | --- | --- | --- |
| `passed` | yes | yes | Reward met the benchmark pass threshold. |
| `failed` | yes | yes | Agent finished, but the verifier rejected the result. |
| `agent_error` | no | no | Agent/model/tool execution failed before verification. |
| `infrastructure_error` | no | no | Harness, sandbox, dataset, or verifier infrastructure failed. |
| `not_run` | no | no | Planned cell did not execute. |
| `not_applicable` | no | no | The production Agent lacks a required capability. |

Harbor's terminal display can show a numeric mean even when no trial was
scored. Always read `Trials` and `Exceptions` with `Mean`. For example,
`Trials=0`, `Exceptions=1`, `Mean=0.000` is **not** a zero capability score; it
is an unscored exception. Sylvander's normalized scorecard must keep the mean
reward null in that case.

## Scorecard, not one universal score

Different suites test different environments and reward contracts. Sylvander
therefore publishes a vector rather than averaging unrelated rewards:

1. task capability: executed count, pass rate, mean verifier reward, and
   `pass@k` within each exact suite/version;
2. reliability: Agent errors, infrastructure errors, timeout rate, malformed
   tool-call rate, and recovery rate;
3. efficiency: wall time, model iterations, tool calls, input/output/cache
   tokens, and cost when the provider supplies auditable pricing;
4. coverage: required, executed, not-run, and not-applicable cells;
5. stability: repeated-run variance and worst-task failure concentration.

`pass rate = passed / (passed + failed)`. Agent and infrastructure errors are
reported beside it and never silently added to the denominator. A release gate
also requires a minimum execution coverage, so excluding errors cannot improve
the reported result. `pass@k` is reported only when every task has at least `k`
independent runs under an identical coordinate.

Scores are comparable only when the dataset revision, task set, Agent revision,
provider/protocol/model binding, resource limits, timeout, prompt/tool catalog,
and repetition count match. Dirty-worktree runs are diagnostic evidence, not
release baselines.

## Benchmark portfolio

No single suite represents general Agent quality. The portfolio is selected by
production capability and keeps unsupported domains visible.

| Suite | What it measures | Primary metric | Sylvander state |
| --- | --- | --- | --- |
| Terminal-Bench | Long-horizon terminal work across build systems, debugging, security, data, systems, ML, and scientific workflows | task verifier reward / accuracy | Harbor adapter runs the real Agent today |
| SWE-bench Verified | Repository understanding and a correct patch for real GitHub issues | resolved issue rate | prediction export exists; official execution baseline pending |
| τ³-bench text | Policy-following customer service with user turns and domain tools | domain task reward and `pass@k` | blocked on external tool suspension/resume |
| AgentBench FC | Cross-domain OS, database, knowledge graph, WebShop, and ALFWorld function calling | per-environment success | candidate after generic external tool bridge |
| OSWorld 2.0 | Multimodal desktop perception, grounding, and long-horizon computer use | task success rate | not applicable until production GUI/computer-use port exists |
| WebArena/VisualWebArena | Stateful website navigation and browser action | task success rate | not applicable until production browser port exists |

Terminal-Bench 2.0 contains 89 independently verified tasks. The repository's
representative matrix intentionally samples multiple families; a single task
such as `gpt2-codegolf` is only an adapter smoke test. The current upstream
release is Terminal-Bench 2.1, which fixes 28 of the 89 version-2.0 tasks, so
new release baselines should migrate to 2.1 after its exact dataset revision is
pinned. See the [official Harbor tutorial](https://www.harborframework.com/docs/tutorials/running-terminal-bench)
and [Terminal-Bench 2.1 release notes](https://www.tbench.ai/news/terminal-bench-2-1).

SWE-bench Verified contains 500 human-screened issues and is narrower than a
general software-engineering evaluation. It is retained because it tests a
different artifact from Terminal-Bench: a repository patch that passes the
official per-instance tests. See the [SWE-bench Verified description](https://openai.com/index/introducing-swe-bench-verified/)
and [official harness](https://github.com/SWE-bench/SWE-bench).

τ³-bench adds user interaction, policies, and domain-owned tools across retail,
airline, telecom, and knowledge retrieval. Directly calling its model adapter
would measure the model rather than Sylvander, so those cells remain explicit
capability gaps until the production Agent can suspend and resume an external
tool call. See the [official τ³ repository](https://github.com/sierra-research/tau2-bench).

OSWorld and browser suites are likewise not emulated through shell commands.
They become required only after matching production capabilities exist. See
the [OSWorld 2.0 release contract](https://github.com/xlang-ai/OSWorld-V2) and
[AgentBench FC environments](https://github.com/THUDM/AgentBench).

## Sylvander capability levels

The final report assigns the highest level whose required evidence is complete.
This is a release gate, not a weighted leaderboard score: a missing required
suite or excessive execution-error rate caps the level even if another suite
is strong.

| Level | Interpretation | Required evidence |
| --- | --- | --- |
| L0 | conversational only | no independently verified tool baseline |
| L1 | basic tool Agent | at least 80% required-cell execution coverage, under 20% Agent errors, and at least one passing Terminal-Bench regression task |
| L2 | qualified terminal/coding Agent | at least 95% coverage, under 10% Agent errors, Terminal-Bench regression pass rate at least 30%, and SWE-bench regression pass rate at least 15% |
| L3 | reliable long-horizon engineering Agent | full Terminal-Bench and SWE-bench release suites, under 5% Agent errors, Terminal-Bench pass rate at least 50%, SWE-bench Verified resolved rate at least 30%, and no material repeated-run regression |
| L4 | general tool-interaction Agent | L3 plus τ³ text and AgentBench FC, at least 50% τ³ domain reward, at least 40% AgentBench FC macro success, and no required domain below 25% |
| L5 | release-grade general Agent | L4 plus at least 70% Terminal-Bench, 50% SWE-bench Verified, 70% τ³, 60% AgentBench FC, under 2% Agent errors, and stable three-run confidence bounds |

The thresholds are Sylvander engineering gates, not claims that the upstream
projects define these levels. The report includes each raw upstream metric,
coverage, error taxonomy, efficiency, repetition variance, and the limiting
criterion. A level based only on a regression subset is marked `provisional`;
only full pinned suites can produce a `release` level.

## Execution profiles

The same tasks must not be used for every development decision:

| Profile | Purpose | Required shape |
| --- | --- | --- |
| adapter smoke | prove packaging, sandbox, model call, tools, trajectory, and verifier wiring | 1–3 tasks, one run; never published as an Agent baseline |
| pull-request regression | detect obvious policy/tool regressions cheaply | stratified fixed subset, one run, no new infrastructure errors |
| nightly | measure stability and diagnose failures | larger stratified subset, at least three runs per task |
| release baseline | compare Agent revisions | full pinned suite, upstream repetition policy, clean commit |

The in-repository `mainstream.example.json` is a representative regression
profile: 15 Terminal-Bench tasks spanning distinct workflow families and 12
SWE-bench Verified issues spanning distinct repositories, each repeated three
times for every deployment. It is not a replacement for either full release
suite.

## The 2026-08-13 observable smoke run

The real MiniMax/OpenAI-Chat/`MiniMax-M2.7` run on Terminal-Bench 2.0
`gpt2-codegolf` executed 25 model iterations and 35 successful Command calls.
Its observable trajectory recorded 90,363 prompt tokens, 11,603 completion
tokens, 20,070 cache-read tokens, provider response IDs, compression events,
and the terminal error. The final model stream contained invalid JSON tool
arguments, so the verifier never ran.

Consequently this run has:

- capability reward: **not available**;
- Harbor scored trials: **0**;
- Agent execution errors: **1** (`invalid_tool_arguments`);
- observability/conformance evidence: **passed** for incremental, redacted,
  interruption-safe trajectory capture.

It proves that the real Sylvander Agent, production provider adapter, tools,
sandbox, and observability path executed. It does not establish a Terminal-
Bench capability baseline.
