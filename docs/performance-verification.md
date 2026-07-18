# Local performance verification

The current release scope is the local Agent service, local TUI, and
host-backed container/sandbox execution. Run its repeatable gate with:

```sh
./scripts/performance-verify.sh
```

The script first produces a locked release build, then prewarms exact isolated
test binaries under `target/performance` so editor or background Cargo locks do
not contaminate interaction measurements. Every measured case has a ten-second
command budget; the implementation uses much smaller internal limits where
waiting correctness matters.

## Budgets and evidence

| Area | Workload and invariant | Release budget |
|---|---|---|
| Runtime concurrency | 8 publishers deliver 4,000 messages to one bounded subscriber with no loss or false backpressure | 10 s command; consumer has an internal 5 s timeout |
| Large workspace | recursively list and search 2,500 files, stopping at 200 retained results with explicit truncation | 10 s |
| Agent tool scheduling | independent ordinary tools start concurrently | 10 s |
| Tool output burst | 1,000 progress updates remain bounded and emit exactly one omission marker | 10 s |
| Long TUI session | over-budget transcript entries and bytes prune to 2,000 entries / 16 MiB with a visible notice | 10 s |
| Input responsiveness | 100,000 redundant redraw intents remain bounded and a later key is delivered | 10 s |
| Service burst | the 1,024-event client queue reports backpressure rather than growing | 10 s |
| Executor resources | invalid container/sandbox memory, CPU, and process limits fail startup validation | 10 s |

On 2026-07-16 the gate passed on the development Mac. Measured command times
after isolated prewarming were 0–2 seconds; the release workspace build also
passed. These are regression budgets, not a claim that model-provider latency
is locally controllable.

## Runtime limits

- the TUI renders keyboard input immediately and otherwise coalesces at 60 FPS;
- service and input drains are capped at 256 and 64 events per cycle;
- socket, terminal input, transcript, tool output, attachment, prompt, and
  overlay queues all have explicit ceilings;
- workspace queries cap results, line width, output bytes, and wall time;
- local commands concurrently drain bounded stdout/stderr and kill their
  process group on timeout or cancellation;
- container and sandbox operations have configured memory, CPU, PID, temporary
  storage, and operation deadlines.

Detailed TUI scheduling is specified in
[`../sylvander-tui/docs/INPUT-RENDERING.md`](../sylvander-tui/docs/INPUT-RENDERING.md).

## Deployment-specific measurements

The default deterministic gate does not claim an SSH latency or transfer SLO
because those values belong to the selected host and network. Executor unit
tests cover bounded dual-stream capture and cancellation; the opt-in real-SSH
journey covers remote process-group cancellation and durable worktree restart,
review, and acceptance. A deployment must record its own latency and transfer
budgets when enabling an SSH target. Provider round-trip time and platform
webhook delivery are likewise external-service SLOs; local channel burst
queues, replay bounds, and Agent scheduling remain covered here and in their
adapter tests.
