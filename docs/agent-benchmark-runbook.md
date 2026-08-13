# Agent benchmark runbook

## Purpose and hard constraints

This runbook produces independently verified Sylvander Agent evidence. On an
Apple Silicon host, every locally scored container must execute natively as
`linux/arm64`; QEMU and Rosetta emulation are forbidden. A run that violates
this rule must stop before the first model request and cannot affect a
waterline.

The execution coordinate always includes benchmark and dataset revision, task,
Agent commit, provider, protocol, model, run ordinal, host architecture, image
architecture and image digest. API keys enter only through process environment
and never enter Git or normalized results.

## Image decision flow

1. Resolve the task at its immutable upstream revision.
2. Prefer its upstream multi-architecture image when the selected manifest is
   `linux/arm64`.
3. If upstream has no arm64 image, build a native image from the pinned task
   Dockerfile, setup scripts and dependency locks. Do not translate or run an
   amd64 layer.
4. Record source revision, Dockerfile SHA-256, build command, base-image digest,
   final-image digest, `podman image inspect` architecture and build log.
5. Run the task's reference/gold solution and original verifier without an
   Agent. The image is eligible only when this passes repeatedly.
6. Run the unmodified baseline when the benchmark defines fail-to-pass and
   pass-to-pass tests. A verifier that passes everything before the fix is
   invalid.
7. Only after these gates may a billable Sylvander run start.

An arm64 rebuild is a Sylvander-native diagnostic/regression environment. It is
not relabelled as an upstream prebuilt image. Scores compare only with runs
using the same recorded image digest.

## Native architecture preflight

The Podman machine itself is arm64. Every Harbor invocation adds:

```sh
--ae SYLVANDER_HARBOR_REQUIRED_ARCH=aarch64 \
--ae SYLVANDER_HARBOR_BINARY_HOST_PATH=/path/to/linux-aarch64/sylvander-harbor-agent
```

Before a live run, verify:

```sh
podman image inspect IMAGE --format '{{.Os}}/{{.Architecture}} {{.Digest}}'
podman run --rm IMAGE uname -m
```

Accepted output is `linux/arm64` and `aarch64`. `linux/amd64`, `x86_64`, a
missing architecture or a failed probe stops the run. The Harbor adapter repeats
this check inside the actual task container, before runner upload and model use.

## Gold/reference qualification

For every locally rebuilt image, create a qualification job with the exact task
tests and reference solution. Retain:

- build inputs and digests;
- verifier stdout/stderr and reward;
- host/image architecture;
- network policy and any external endpoints;
- wall time and exit status.

External services make a qualification non-clean unless they are benchmark-
owned, pinned and health-checked. HTTP 429/5xx, DNS failures, package-index
drift, QEMU signals and verifier crashes are infrastructure errors. They are not
Agent failures, even when the upstream harness emits a numeric zero.

## L1 execution procedure

L1 is the first independently verified tool-using level. The shortest clean
path is:

1. select three small, deterministic Terminal-Bench regression tasks from
   different capability families;
2. build or resolve native arm64 images and pass the reference qualification;
3. run one fixed Sylvander commit with one fixed provider/protocol/model binding;
4. retain ATIF trajectory, Harbor result and normalized record for every task;
5. require at least 80% planned-cell execution coverage, under 20% Agent errors
   and at least one clean verifier pass;
6. repeat the passing task three times before calling L1 stable.

Recommended first candidates are `cancel-async-tasks` (Python concurrency),
`large-scale-text-editing` (constrained file transformation) and
`sanitize-git-repo` (Git/security hygiene). Their upstream base images are
official Python arm64-capable images, and their verifiers do not inherently
need an external HTTP service. Image manifest and gold qualification still
remain mandatory.

The first qualified image is `cancel-async-tasks` at task revision
`69671fbaac6d67a7ef0dfec016cc38a64ef7a77c`, native image digest
`sha256:fa17b9590f1fe4aa1623fe906e867ecaa29bbdbbeed116acb2544e8cffaad5f2`.
Its gold solution and first Sylvander run both passed all six verifier cases.

## Staged baseline loop

Use `sylvander-benchmark-agent/harbor/build_native_runner.py` followed by
`run_native_benchmark.py`. The runner sidecar binds its SHA-256 and architecture
to the exact Agent commit; the run script refuses drift. Execute `smoke`,
`small`, `medium`, then `large`. A verifier failure, Agent error, or timeout is
baseline data and does not stop the round. Stop only for an isolation,
credential, architecture, corrupt-evidence, or harness failure. Do not change
Agent code or model within a round.

Analyze two axes separately after the round:

- Agent axis: fix provider, protocol, model, task image, and dataset; compare
  Agent commits for success, reliability, steps, commands, tokens, and latency.
- Model axis: fix Agent commit, provider, protocol, task image, and dataset;
  compare models using the same metrics.

The interaction is reported explicitly, but a model gain is never attributed
to Agent design and an Agent gain is never attributed to the model.

## Result classification

| Condition | Classification | Waterline use |
| --- | --- | --- |
| Agent completes and verifier accepts | `passed` | yes, if environment is clean |
| Agent completes and clean verifier rejects | `failed` | yes |
| Agent/model/tool fails before verifier | `agent_error` | reliability only |
| image, network, harness or verifier fails | `infrastructure_error` | no |
| planned cell not started | `not_run` | coverage penalty |
| production Agent lacks required capability | `not_applicable` | caps portfolio level |

Raw upstream reward is never rewritten. Environment eligibility is a separate
field in the human scorecard until it becomes part of the normalized schema.

## Evidence and closeout

After each job, normalize the Harbor result and ATIF trajectory, scan tracked
files and artifacts for credentials, and run the benchmark module tests,
strict Clippy and warning-denied Rustdoc. Update the scorecard only from clean
committed evidence. See [agent-benchmark-cases.md](agent-benchmark-cases.md) for
the fixed regression tasks and [agent-benchmark-scorecard.md](agent-benchmark-scorecard.md)
for L0–L5 gates.
