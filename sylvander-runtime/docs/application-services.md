# Runtime application services

## What Runtime owns

Runtime is Sylvander's application layer and composition root. It turns an
authenticated API operation into durable product state and, when needed, one
bounded Agent execution. Its internal services are Session, Agent supervision,
authorization, execution, storage, and observability.

These are product services, not public extension points. Their traits remain
crate-private unless a separate reviewed interoperability boundary is created.

## Session and Agent supervision

The Session service owns product Session identity, ownership, configuration
revision, transcript, turn admission, resume, archive, and interruption. The
Agent supervisor serializes work per Session, constructs one
`AgentTurnRequest`, consumes `AgentEvent`, and submits one `AgentOutcome` to the
storage transaction. It maps internal events to API events only after applying
product visibility and persistence rules.

## Execution service

The execution service maps Agent logical workspace and target identifiers to
concrete workspace and sandbox adapters. Agent is trusted control-plane code
outside the sandbox; model-triggered processes are isolated data-plane work.

The service owns process launch, filesystem mounts, network enforcement,
resource ceilings, cancellation, bounded output, artifact collection,
violations, and cleanup. OCI is the current enforcing adapter. Local and SSH
remain non-sandboxed adapters and cannot execute a tool whose prepared policy
requires a sandbox.

Current implementation status: Runtime boot constructs one crate-private,
immutable `RuntimeExecutionService` from the built-in exact `local` target and
configured SSH/OCI targets. It resolves adapter credentials at composition,
rejects invalid target registries, and shares the same service with initial
and lazily recomposed Agent revisions. Unknown target identifiers receive an
explicit unavailable executor; they never fall back to `local`. Worktree
lifecycle is still a neighboring Runtime service rather than part of this
registry. The operational snapshot exposes a deterministic content-free target
list with adapter kind, `ready` or `unverified` status, each enforced isolation
property, and the derived full-sandbox result. `Unverified` is never presented
as successful reachability.

Every registered executor is wrapped by Runtime's workspace coordinator.
Coordination is keyed by exact target identity and workspace path: reads share
access, while ordinary writes, conditional writes, and arbitrary commands are
exclusive across Sessions. `Edit` receives a content revision from its update
read; the coordinator re-reads under the exclusive lock and rejects stale or
truncated state before delegating the write. This boundary belongs here rather
than in Agent scheduling because two product Sessions can mount the same
physical workspace. It is process-local and does not claim to govern writers
that bypass Runtime.

Local, OCI, and SSH file writes stage bytes in a uniquely named file in the
destination directory and rename only after the full input is written. Existing
ordinary permission bits are preserved. Failure and signal paths remove the
staging file. This gives readers atomic replacement visibility on conforming
filesystems; it does not claim cross-device behavior or durable directory
metadata after host power loss.

Execution-policy violations are typed only from backend-owned evidence. The
OCI adapter's fixed filesystem scripts reserve exit status 126 for a resolved
path crossing the workspace mount, and only that path becomes a neutral
`FilesystemBoundary` violation. Runtime does not label arbitrary command
failures, `EPERM` text, or container exit codes as sandbox violations. Linux
network/syscall attribution remains unimplemented until the selected backend
can emit a correlated, trustworthy signal.

Runtime now owns one bounded health worker for that service. Every 30 seconds
it probes SSH targets with the executor's exact BatchMode, strict known-host,
identity, and control-socket arguments and a fixed remote `true`; OCI targets
use a fixed `image inspect` for the configured runtime and exact image. Probes
run concurrently with a five-second hard timeout, discard all output, retain
only the last success bit and a saturating failure count, and are joined during
shutdown. A failed probe marks the target degraded and Runtime unready; a later
successful probe restores readiness without erasing the historical count.

## Unified storage

The target is one closed Runtime storage facade owning sessions, messages,
runs, turns, steps, usage, artifacts, approvals, audit, registries, profiles,
memory, and evidence. Services will request repository operations through one
transaction rather than opening SQLite independently. Schema namespaces may
retain separate integrity fingerprints, but backend lifecycle, connection,
transaction, backup, and health ownership must be unified.

The initial backend is built-in SQLite. There is no storage plugin registry or
public backend trait.

Current implementation status: the crate-private `RuntimeStorage` composition
root owns the exact Session and relationship-memory repository handles selected
at boot. `Runtime` no longer exposes either repository as a public field.
Session schema v2 stores explicit turn lifecycle. Turn admission is atomic
with user input and immutable configuration; successful completion is atomic
with assistant output.

Production composition also retains concrete, non-extensible health probes in
this facade. Each operational request checks Session, relationship memory,
Agent Registry, User Profiles, Evidence, credential-operation audit, Guardian
curation, and Guardian canonical memory concurrently. The Session probe
verifies its exact live schema, `SQLite`
quick-check, and owned foreign keys. The memory
probe verifies its exact live schema, `SQLite` quick-check, and the independent
authenticated integrity anchor when configured. Registry verifies the exact
shared namespace, current schema ledger, owned foreign keys, and `SQLite`
pages; User Profiles verify their exact schema and `SQLite` pages. Evidence
verifies its exact base schema, page/foreign-key integrity, and governed table
shape when enabled. Artifact health independently verifies that same governed
store only when encrypted retention is configured; otherwise it is
`unverified` and Agent compression keeps the only inline copy. Credential audit verifies its exact schema and database
integrity. Guardian's two stores reject non-current object sets both at open
and during health; curation also verifies foreign keys. RuntimeStorage receives
a health-only pair of cloned store handles, not the supervisor or its
credential/mutation authority. Public results contain only component identity and
`ready`, `unverified`, or `degraded`; raw errors and storage locations remain
private. Any degraded component, or a failed Session-count read, contributes
the stable `Storage` health issue and makes Runtime unready. `Unverified` is
valid only when a component is intentionally absent, including disabled
artifact governance, or in isolated composition; it is never success for a
required configured store.

Turn artifacts now enter one Runtime-owned encrypted service bound to the
authenticated user, Agent, Session, and turn. Agent receives only a
location-neutral port and opaque locator. There is not yet an authorized
retrieval service, cross-domain transaction, or unified backup lifecycle;
callers must not infer those target capabilities today.

Evidence has two deliberately separate health facts. Database/schema failures
are part of the unified `Storage` issue. The asynchronous recorder's sticky
write failure remains `EvidenceRecorder`, because database recovery cannot
retroactively restore an omitted governance fact.

Guardian follows the same separation. Curation/canonical database failures are
`Storage`; polling, retry, or worker-loop failure is `GuardianSupervisor`.
Tests corrupt the live canonical schema and prove the supervisor remains
healthy while Runtime becomes unready for Storage alone.

## Built-in observability

The target is for Runtime to assign correlation identifiers and emit one typed
internal lifecycle event for admitted, started, retried, authorized, executed,
persisted, published, interrupted, and failed states. Built-in tracing,
metrics, durable evidence, and health views consume those facts.
Observability sinks are not runtime extensions at this stage.

Current implementation status: the closed `RuntimeObservability` recorder
consumes typed, content-free ingress, turn, retry, tool, persistence, and
terminal facts. One recorder is composed at Runtime boot and injected into
every current and lazily recomposed Agent revision. It updates built-in
counters, active dispatch/turn/tool gauges, bounded dispatch/turn/tool latency
histograms, and unmatched-terminal diagnostics; all are exposed through the
operational snapshot. Existing durable evidence remains a separate path;
it is an asynchronous governance projection rather than Session commit
authority. The durable Session terminal commits before the matching built-in
terminal fact and public event. Cross-restart metric aggregation,
sink-failure health, and CPU/memory/network resource histograms remain
incomplete.

### Design evidence

The lifecycle vocabulary was checked against pinned local implementations:

- Codex `16fbfe557446a1af94da81e1144029ccc1311ad0`, especially
  `sdk/typescript/src/events.ts` and
  `codex-rs/code-mode-runtime/src/runtime/mod.rs`, separates turn terminals
  from item/tool terminals and keeps runtime events typed.
- pi `11b5403fade1502a9a58a9cd4e9f983a3d1d734e`, especially
  `packages/agent/src/types.ts`, pairs agent/message/tool start and end facts
  and tests their ordering.
- Claude Code `3da94d5e5f2b99c9d82b0d8f09448b04775cd41f`, especially
  `src/entrypoints/sdk/coreSchemas.ts`, distinguishes successful and failed
  post-tool/stop lifecycle hooks.

Sylvander adopts explicit start/terminal pairing and typed success/failure,
but does not reuse UI messages or extensible hooks as observability. Runtime
adds persistence facts because a product turn is not successful until its
Session writes commit. Facts deliberately omit prompt text, tool input/output,
provider errors, credentials, and user content.

A public success event requires both a committed storage outcome and a terminal
observability fact. Content is excluded by default and governed separately
when capture is enabled.

## Why this layer is above Agent

Agent can fail and retry without owning a durable Session. Runtime cannot
publish success, change ownership, or grant authority without durable and
auditable product rules. Keeping those responsibilities here prevents model
execution concerns from leaking into API, Channel, TUI, or desktop modules.
