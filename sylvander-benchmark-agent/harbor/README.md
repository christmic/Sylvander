# Harbor adapter

This directory contains the thin Harbor-side adapter. It requires a Linux build
of `sylvander-harbor-agent` at
`/opt/sylvander/bin/sylvander-harbor-agent` in the Agent task image. Alternatively,
set `SYLVANDER_HARBOR_BINARY_HOST_PATH` to a prebuilt Linux executable; Harbor's
environment transport uploads it during `setup`. The adapter does not install
compilers or mutate the benchmark dataset at runtime.

Run it as a custom Harbor Agent using the pinned source contract:

```sh
harbor trials start \
  -p path/to/task \
  --agent-import-path sylvander_agent:SylvanderAgent \
  -m minimax-cn/MiniMax-M2.7 \
  --agent-env SYLVANDER_HARBOR_API_KEY \
  --agent-env SYLVANDER_HARBOR_BASE_URL=https://api.minimaxi.com/v1
```

`SYLVANDER_HARBOR_PROTOCOL` selects `anthropic_messages`,
`openai_responses`, `openai_chat_completions`, or `dashscope_generation`.
Provider-specific compatibility switches are a comma-separated
`SYLVANDER_HARBOR_PROVIDER_FEATURES` value and are validated by that selected
protocol adapter, never inferred from the model name.

The uploaded runner must match the task image architecture and should be a
static musl executable. During `setup`, Harbor executes `--self-check`; an
install-only gate therefore fails before scoring if the image cannot load the
binary.

Add `sylvander-benchmark-agent/harbor` to Python's import path. Credentials are
passed through Harbor's Agent environment mechanism and never appear in the
command, final answer, or evidence. The detailed trajectory stores only a
truncated SHA-256 fingerprint so an authorized call can be correlated without
persisting the raw credential.

The adapter writes the files Harbor expects:

- `/logs/agent/trajectory.json` — ATIF v1.7;
- `/logs/agent/final_answer.txt` — final user-visible Agent message.

The Rust runner writes `trajectory.json` atomically throughout execution, not
only after a successful terminal event. It is therefore a valid partial ATIF
artifact after Harbor timeout, runner interruption, or tool-executor
cancellation. `extra.sylvander_observability` is the ordered projection of the
Agent's public `AgentEvent` stream: request response IDs, retries, tool
lifecycle, per-request token/cache usage, and terminal state. Text/reasoning
deltas and tool output are checkpointed at lifecycle boundaries to avoid
per-token filesystem I/O.

If the runner exits non-zero, the adapter retains a bounded stdout/stderr
diagnostic in Harbor's exception artifact and replaces the active API key with
`[REDACTED]` before persistence.

Agent terminal errors retain their provider-neutral error chain rather than
collapsing every failure to one generic message. This is diagnostic evidence,
not a verifier result.

Harbor owns environment isolation and verifier reward. The Rust runner owns the
Agent execution and trajectory. This Python layer owns only lifecycle bridging.

## Podman sandbox

The verified local backend is rootless Podman. Harbor still calls its
Docker-compatible environment interface, so put this directory first on
`PATH`; its `docker` entrypoint invokes `podman_compat.py`. The wrapper forwards
every command to Podman. Its only translation is Compose's
`--project-directory`: the wrapper removes that unsupported `podman-compose`
flag and uses its value as the Compose process working directory.

On Apple Silicon, local benchmark containers must be native `linux/arm64`.
Set `SYLVANDER_HARBOR_REQUIRED_ARCH=aarch64`; setup checks `uname -m` before
installing the runner or making a model call and fails closed on amd64. QEMU is
not an allowed fallback. If upstream has no arm64 image, build one from the
pinned task sources, record its inputs and digests, and require the reference
solution to pass before running Sylvander.

The reference environment uses Podman client 6.1.0, Podman server 6.0.2, the
`quay.io/podman/machine-os:6.0` AppleHV machine image, and podman-compose 1.6.0.
Isolation checks cover no-network execution, a read-only filesystem, zero Linux
capabilities, `no-new-privileges`, CPU/memory/PID limits, read-only workspace
mounts, and exact exit-code propagation.

The Sylvander custom Agent path does not use LiteLLM. A minimal Harbor install
may omit it: provider requests are made by `sylvander-harbor-agent` through the
same production protocol adapters exercised elsewhere in this repository.

Use `run_native_benchmark.py` for local runs instead of assembling Harbor
commands manually. It provides staged `smoke`, `small`, `medium`, and `large`
levels; verifies Podman is native arm64 with Rosetta disabled; verifies the
runner and task image architecture; refuses to reuse a job directory; and
scans only that job's artifacts for the active credential. `smoke` is an
install-only setup gate and does not require a key. Scored levels read the key
from `SYLVANDER_BENCH_API_KEY`; the variable is removed from Harbor's process
environment and the value is passed through a mode-0600 temporary config:

```sh
python3 sylvander-benchmark-agent/harbor/run_native_benchmark.py smoke
SYLVANDER_BENCH_API_KEY='...' \
  python3 sylvander-benchmark-agent/harbor/run_native_benchmark.py small
SYLVANDER_BENCH_API_KEY='...' \
  python3 sylvander-benchmark-agent/harbor/run_native_benchmark.py small \
  --model MiniMax-M3
```

Advance one level only after the preceding level finishes cleanly. `large`
contains the million-row stress case and is intentionally last.
Every model ID creates a separate benchmark coordinate and job name; results
from different models must not be combined into one Agent-revision baseline.

Build and attest a replacement runner with `build_native_runner.py`. The build
refuses a dirty worktree and non-native Podman machine, performs an arm64 static
ELF and container self-check, and writes sidecar metadata binding the binary
SHA-256 to its exact Git commit. The run script refuses binaries without
matching metadata, so the scored Agent revision cannot drift from repository
HEAD.

The effective 2026-08-13 install-only gate used a static x86-64 musl runner and
completed the Terminal-Bench 2.0 `gpt2-codegolf` task with zero exceptions in
18 seconds. It started the Podman container, uploaded the runner via Harbor's
tar fallback, executed the setup self-check, and cleaned the environment. An
earlier 65-second gate only checked file presence and did not prove its arm64
runner could execute in the amd64 task image; it is superseded. Install-only
does not call a model or produce a verifier score.
