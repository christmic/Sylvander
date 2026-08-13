# Workspace execution contract

Built-in coding tools depend on `WorkspaceExecutor`, not on local paths,
OpenSSH, containers, or presentation code. A `WorkspaceTarget` selects one
backend workspace; `WorkspaceRouter` maps stable `@reference/path` names to
Agent-home, task, dependency, and artifact mounts with independent
capabilities.

## Operations

The contract provides:

- full and bounded file reads, ordinary writes, and revision-bound conditional
  writes;
- deterministic bounded list and text search;
- ordinary and streaming command execution;
- command-scoped environment overrides;
- a separate structured read-only command boundary used by Git inspection.

Read/Write/Edit/List/Search/Command/Git all receive the effective executor and
target through `ToolContext`. Their constructors are zero-argument and retain
no path state. An empty workspace or unknown target fails explicitly and never
falls back to the process directory or a same-named host path.

## Mutation consistency

`Edit` is a read-modify-write operation, so an ordinary `write_file` is not a
valid commit primitive. It first calls `read_file_for_update`, which returns
the complete bounded bytes plus an opaque `WorkspaceFileRevision`, then calls
`write_file_if_revision`. Agent can forward and compare this content revision
but cannot derive filesystem metadata or a storage location from it.

The conditional-write default fails closed. Runtime's execution service wraps
every concrete target with a coordinator keyed by execution-target identity
and workspace path. Reads take a shared lock; writes and arbitrary commands
take the exclusive lock. A conditional write takes the exclusive lock,
re-reads the file, rejects truncation or revision drift, and only then invokes
the concrete write. This prevents two Sessions sharing one workspace from both
committing edits prepared from the same bytes. A change made outside Runtime
before revalidation is also rejected.

The coordinator is process-local. Deployment policy must not let an unrelated
writer bypass Runtime when exclusive ownership is required. Atomic replacement
and crash durability of the final write remain properties of each concrete
filesystem adapter; the revision contract does not invent those guarantees.
Runtime's current local, OCI, and SSH adapters stage in the destination
directory and rename after a complete write, preserving existing ordinary
permission bits. They do not claim durable directory metadata after host power
loss.

Command environment overrides are limited to 64 entries. Names must use shell
identifier syntax, names are at most 128 bytes, values are at most 8 KiB, and
NUL is rejected. Runtime's local adapter overlays accepted values only for the
spawned command. Backends that do not implement overrides reject a non-empty
map instead of silently dropping it.

## Bounds and cancellation

Query result count, line width, output bytes, and duration are clamped by the
executor. Runtime's local adapter concurrently drains stdout and stderr,
preserves a bounded head/tail with exact totals, and emits Unicode-safe
progress. Each command runs in its own process group. Timeout or future
cancellation terminates the whole group so descendants cannot outlive the
Agent turn.

`ProcessIsolation::enforces_sandbox()` requires four independent truths:
filesystem isolation, denied network, resource ceilings, and owned process-tree
cleanup. Missing any one dimension fails closed for a prepared call whose
sandbox is required. This matches the persistent-process environment contract;
a backend cannot claim a complete sandbox merely because its filesystem mount
is restricted.

Agent tests cover executor injection, logical mount routing, prepared-policy
bounds, capability denial, conditional Edit behavior, and unavailable-target
behavior through test doubles. Runtime tests prove that concurrent stale
revisions have exactly one winner and that a detected out-of-band change is
preserved. Runtime also owns concrete `LocalExecutor` conformance: file
read/write, bounded reads, list/search, environment, streaming, read-only
enforcement, output pressure, UTF-8 chunk boundaries, timeout, and dropped
future cancellation.

The OpenSSH executor uses strict host-key verification, a deployment-owned
known-hosts file, bounded control connection reuse, and a remote process-group
wrapper. Timeout, interrupt, or dropped execution futures terminate the
transport and the owned remote group. Remote Git worktrees use durable local
lease manifests plus create, inspect, accept, discard, and restart
reconciliation against the configured remote worktree root. The opt-in
real-SSH journey is the deployment acceptance gate because it requires a
disposable SSH daemon and repository. Container resource policy and managed
sandboxes use the same Agent-facing contract.

## Policy violations

The neutral executor error contract distinguishes a policy violation only when
the physical backend provides an explicit, trustworthy signal. OCI's fixed
file-operation scripts reserve exit status 126 exclusively for a canonical
path crossing the mounted workspace boundary, so Runtime maps only that status
to `WorkspacePolicyViolation::FilesystemBoundary`. Other non-zero statuses,
stderr text, and generic permission errors remain ordinary operation failures.

This conservative rule is intentional. Linux container and syscall sandboxes
do not universally provide attribution for denied operations, and treating an
arbitrary `EPERM` or exit code as a violation would fabricate observability.
Future native adapters may add kinds only together with a backend-owned signal,
per-invocation correlation, and tests proving that ordinary command failures
cannot be misclassified.
