# Workspace execution contract

Built-in coding tools depend on `WorkspaceExecutor`, not on local paths,
OpenSSH, containers, or presentation code. A `WorkspaceTarget` selects one
backend workspace; `WorkspaceRouter` maps stable `@reference/path` names to
Agent-home, task, dependency, and artifact mounts with independent
capabilities.

## Operations

The contract provides:

- full and bounded file reads plus bounded writes;
- deterministic bounded list and text search;
- ordinary and streaming command execution;
- command-scoped environment overrides;
- a separate structured read-only command boundary used by Git inspection.

Read/Write/Edit/List/Search/Command/Git all receive the effective executor and
target through `ToolContext`. Their constructors are zero-argument and retain
no path state. An empty workspace or unknown target fails explicitly and never
falls back to the process directory or a same-named host path.

Command environment overrides are limited to 64 entries. Names must use shell
identifier syntax, names are at most 128 bytes, values are at most 8 KiB, and
NUL is rejected. The local executor overlays accepted values only for the
spawned command. Backends that do not implement overrides reject a non-empty
map instead of silently dropping it.

## Bounds and cancellation

Query result count, line width, output bytes, and duration are clamped by the
executor. Local commands concurrently drain stdout and stderr, preserve a
bounded head/tail with exact totals, and emit Unicode-safe progress. Each
command runs in its own process group. Timeout or future cancellation
terminates the whole group so descendants cannot outlive the Agent turn.

The reusable core conformance test covers file read/write, bounded reads,
list/search, environment, ordinary and streaming commands, and the read-only
inspection boundary through both `LocalExecutor` and `WorkspaceRouter`.
Separate regressions cover query limits, read-only mounts, output pressure,
UTF-8 chunk boundaries, timeout, dropped futures, and unavailable targets.

The OpenSSH executor uses strict host-key verification, a deployment-owned
known-hosts file, bounded control connection reuse, and a remote process-group
wrapper. Timeout, interrupt, or dropped execution futures terminate the
transport and the owned remote group. Remote Git worktrees use durable local
lease manifests plus create, inspect, accept, discard, and restart
reconciliation against the configured remote worktree root. The opt-in
real-SSH journey is the deployment acceptance gate because it requires a
disposable SSH daemon and repository. Container resource policy and managed
sandboxes use the same Agent-facing contract.
