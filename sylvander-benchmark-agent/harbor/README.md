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

Add `sylvander-benchmark-agent/harbor` to Python's import path. Credentials are
passed through Harbor's Agent environment mechanism and never appear in the
command, trajectory, final answer, or aggregate evidence.

The adapter writes the files Harbor expects:

- `/logs/agent/trajectory.json` — ATIF v1.7;
- `/logs/agent/final_answer.txt` — final user-visible Agent message.

Harbor owns environment isolation and verifier reward. The Rust runner owns the
Agent execution and trajectory. This Python layer owns only lifecycle bridging.

## Podman sandbox

The verified local backend is rootless Podman. Harbor still calls its
Docker-compatible environment interface, so put this directory first on
`PATH`; its `docker` entrypoint invokes `podman_compat.py`. The wrapper forwards
every command to Podman. Its only translation is Compose's
`--project-directory`: the wrapper removes that unsupported `podman-compose`
flag and uses its value as the Compose process working directory.

The reference environment uses Podman client 6.1.0, Podman server 6.0.2, the
`quay.io/podman/machine-os:6.0` AppleHV machine image, and podman-compose 1.6.0.
Isolation checks cover no-network execution, a read-only filesystem, zero Linux
capabilities, `no-new-privileges`, CPU/memory/PID limits, read-only workspace
mounts, and exact exit-code propagation.

The Sylvander custom Agent path does not use LiteLLM. A minimal Harbor install
may omit it: provider requests are made by `sylvander-harbor-agent` through the
same production protocol adapters exercised elsewhere in this repository.

The 2026-08-13 install-only gate completed the Terminal-Bench 2.0
`gpt2-codegolf` task with zero exceptions in 65 seconds. It pulled the task
image, started the Podman container, uploaded the Linux arm64 Agent binary via
Harbor's tar fallback, and cleaned the environment. Install-only does not call
a model or produce a verifier score.
