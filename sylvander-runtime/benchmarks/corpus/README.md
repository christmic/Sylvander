# Sylvander Runtime governance evidence corpus

Two canonical corpus manifests exercise the Runtime cognition activation
gate through `sylvander-benchmark-runtime`'s `sylvander-runtime-bench` binary.
The governance evidence model (RuntimeBenchResult + paired baseline / candidate
plans + ActivationGatePolicy) is a content-free evidence ledger, not a perf
benchmark. Real task e2e coverage lives in `sylvander-runtime/tests/unit/`
and is reported separately on master.

## Manifests

- `corpus-fastslow.json` — candidate profile `fast_slow`, 3 cognitive routing
  text scenarios × 2 repetitions, paired against `primary_only` baseline.
- `corpus-perception.json` — candidate profile `perception_specialist`, 2
  multimodal perception scenarios (image / audio) × 2 repetitions, paired
  against `primary_only` baseline.

The two `supported_family` values in `validate_scenario` are
`cognitive_routing` and `multimodal_perception`. The four other
`ScenarioFamily` variants (`crash_recovery`, `multi_agent_coordination`,
`workspace_concurrency`, `doctor_experiment`) are runtime e2e coverage and
are not part of this activation-gate corpus.

## Synthetic result sources

The baseline and candidate result files in this directory are synthetic
fixtures built from the metrics the Runtime cognition / perception paths
emit during real wiremock-mocked tests in `sylvander-runtime/tests/unit/`.
They are committed to keep the governance harness self-reproducible from
the repository alone; real production runs will overwrite the ledger with
numbers from actual model calls.

## How to reproduce

```
# Validate manifest schema + artifact digests
sylvander-runtime-bench validate-corpus benchmarks/corpus/corpus-fastslow.json
sylvander-runtime-bench validate-corpus benchmarks/corpus/corpus-perception.json

# Generate paired plans (writes JSON to stdout)
sylvander-runtime-bench plan-corpus benchmarks/corpus/corpus-fastslow.json
sylvander-runtime-bench plan-corpus benchmarks/corpus/corpus-perception.json

# Record baseline + candidate into the activation-gate ledger
sylvander-runtime-bench record benchmarks/corpus/ledger.sqlite3 \
    benchmarks/corpus/corpus-fastslow-baseline.json
sylvander-runtime-bench record benchmarks/corpus/ledger.sqlite3 \
    benchmarks/corpus/corpus-fastslow-candidate.json
sylvander-runtime-bench record benchmarks/corpus/ledger.sqlite3 \
    benchmarks/corpus/corpus-perception-baseline.json
sylvander-runtime-bench record benchmarks/corpus/ledger.sqlite3 \
    benchmarks/corpus/corpus-perception-candidate.json

# Activation gate decision per corpus
sylvander-runtime-bench evaluate-corpus \
    benchmarks/corpus/corpus-fastslow.json \
    benchmarks/corpus/corpus-fastslow-baseline.json \
    benchmarks/corpus/corpus-fastslow-candidate.json \
    benchmarks/corpus/policy.json
sylvander-runtime-bench evaluate-corpus \
    benchmarks/corpus/corpus-perception.json \
    benchmarks/corpus/corpus-perception-baseline.json \
    benchmarks/corpus/corpus-perception-candidate.json \
    benchmarks/corpus/policy.json

# Aggregate ledger summary
sylvander-runtime-bench summarize-ledger benchmarks/corpus/ledger.sqlite3
```

## Current canonical scores

| Corpus | pairs | quality_win | reward_gain | token delta | p95 latency | decision |
|---|---|---|---|---|---|---|
| `corpus-fastslow` (FastSlow vs PrimaryOnly) | 6 | 100% | +0.37 | +18.14% | -50.58% | **eligible** |
| `corpus-perception` (PerceptionSpecialist vs PrimaryOnly) | 4 | 100% | +0.45 | **-35%** | -47.82% | **eligible** |

Both candidate profiles are eligible under the current `policy.json`
(minimum_pairs=2, minimum_reward_gain_micros=0, plus the other bounds).
The perception corpus is the strongest: it cuts both p95 latency and
input tokens while winning every pair.
