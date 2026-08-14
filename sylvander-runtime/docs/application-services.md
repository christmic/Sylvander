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

For an executing turn, Runtime reduces typed `TurnTransition` facts into one
content-free `RuntimeTurnSnapshot` per Session. `active_turn_snapshot` exposes
that typed view for diagnostics; it does not parse logs or copy state names.
The snapshot is removed when the Runtime turn finishes, while the ordered
transition facts remain available to observability for post-mortem analysis.
Agent state completion and Runtime product completion remain separate: only
Runtime may declare success after durable Session commit and publication.

Runtime observability is a governance mechanism rather than part of Agent
execution. The mandatory recorder publishes each typed, content-free lifecycle
fact to a bounded broadcast bus after updating built-in tracing, counters, and
timing. Consumers cannot apply backpressure to the execution path; a slow
consumer observes an explicit lag count instead.

When `server.observability.debug_log` is enabled, a governance task subscribes
to that bus and writes one JSON object per line to a unique file under
`<data_dir>/debug`. The file is capped at 16 MiB, contains no prompts, tool
arguments, tool output, credentials, or user content, and is flushed when
Runtime shuts down. Startup retains at most four recognized per-run files and
64 MiB across that UUID-named log namespace; oldest managed files are removed
before a new one is created, while unrelated debug files are untouched.
`debug_observation_log_path` returns the exact file for the current process.
This diagnostic projection is not authoritative storage and does not
participate in Agent success or failure.

The public event protocol deliberately exposes the coarser product lifecycle,
not every Agent-machine phase. Runtime emits `TurnStarted` only after required
turn admission persistence and executable composition have succeeded. Existing
iteration, tool, and interaction events describe work inside that turn;
`Done`, `Error`, and `TurnInterrupted` remain its public terminals, with
`Done` emitted only after the durable assistant message and completed turn are
committed. This follows the separation visible in local Codex app-server
protocol (`TurnStarted`/`TurnCompleted` plus item events, commit
`16fbfe557446a1af94da81e1144029ccc1311ad0`) and Kimi agent-core
(`TurnStarted`/`TurnEnded`, step, and tool events, commit
`93928066dc308052de8c4a48e9c10b2f3dba361b`) without copying either wire
format.

## Execution service

The execution service maps Agent logical workspace and target identifiers to
concrete workspace and sandbox adapters. Agent is trusted control-plane code
outside the sandbox; model-triggered processes are isolated data-plane work.

The service owns process launch, filesystem mounts, network enforcement,
resource ceilings, cancellation, bounded output, artifact collection,
violations, and cleanup. OCI and macOS Seatbelt are enforcing process adapters.
Local and SSH remain non-sandboxed adapters and cannot execute a tool whose
prepared policy requires a sandbox.

Process-sandbox health is the conjunction of filesystem isolation, denied
network, and process-tree ownership. Resource limits remain a separate fact.
Runtime publishes each truth separately and derives `sandbox_enforced`; it
never upgrades a partially isolated adapter. OCI owns the named container tree and force-removes it when a
timeout, cancellation, transport loss, or dropped future prevents ordinary
completion.

Tool failure observation is fact-based. Agent attaches a content-safe
classification to the tool's single terminal only when an adapter-provided
policy denial is preserved. Runtime records that fact independently from the
provider-facing `is_error` result. Timeout interaction events do not create a
second terminal. The built-in snapshot currently counts explicit filesystem-boundary
violations. It does not inspect model-visible error text and is not replaceable
by extensions.

Current implementation status: Runtime boot constructs one crate-private,
immutable `RuntimeExecutionService` from the built-in exact `local` target and
configured SSH/OCI/macOS Seatbelt targets. Seatbelt is selected first; host
local fallback occurs only when the target explicitly enables it and is
reported as `local_fallback`. Runtime resolves adapter credentials at composition,
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
root owns the exact Session, relationship-memory, and encrypted turn-artifact
authorities selected at boot. `Runtime` no longer exposes those authorities as
public fields.
The Session schema stores explicit turn lifecycle. Turn admission is atomic
with user input and immutable configuration; successful completion is atomic
with assistant output.

The latest-only Session schema v3 owns content-free tool lifecycle rows for
`(session, turn, call, tool name)`. Agent emits a content-free preparation fact
before approval; Runtime persists it before the event stream may continue.
Execution start remains a separate fact, so a rejected call is never described
as executed. Runtime then persists exactly one `succeeded`, `failed`, or
`rejected` terminal before publishing observation or client output. A
successful turn cannot commit while any tool row is still running; an
interrupted or failed turn atomically marks its remaining calls `abandoned`.
Tool arguments and results belong in provider-neutral conversation history or
the governed artifact domain, not this operational table.

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
location-neutral port and opaque locator. Runtime boot clones that bounded
factory from `RuntimeStorage`; no sibling service selects another backend.
Messages remain the turn terminal authority, so this ownership closure does
not claim an atomic transaction across the Session and evidence databases.

Artifact retrieval follows the same ownership direction as persistence. The
public protocol carries an opaque locator, the owning Session, and a bounded
byte offset; it never carries a tenant, user, filesystem path, database key,
or backend URL. Channel authenticates transport identity but does not open
storage. Runtime derives the stable user, proves ownership of the requested
Session, resolves the locator inside that exact tenant/user scope, verifies
that the stored artifact provenance is bound to the same Session, and only
then returns a range. A locator that belongs to another user or Session, has
expired, or has been deleted is uniformly not visible; the response must not
reveal which check failed.

One response contains at most 48 KiB of plaintext, encoded as Base64 for the
JSON transport. This produces at most 64 KiB of encoded content and prevents a
16 MiB governed record from becoming one unbounded UI message. The response
also carries media type, total size, offset, next offset, terminal status, and
the digest of the complete plaintext. The governed store may need to decrypt
the complete authenticated ciphertext before slicing because the current
record format uses one AEAD envelope; that bounded internal allocation is not
permission to return the complete record. Each successful range read appends
a content-free governance audit in the same database transaction as the read.

The provenance binding is a stable SHA-256 digest of the Session identifier,
stored as a parseable prefix of the content-safe `source_ref`. Both Agent turn
artifacts and MCP result artifacts use the same prefix; provider-specific
metadata follows only as a digest. Retrieval accepts only the exact supported
locator namespaces and exact provenance format. This is an internal storage
contract, not a public capability for guessing or constructing locators.

Cross-domain transactions and a unified backup lifecycle remain incomplete;
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
terminal fact and public event. The optional bounded debug projection reports
any post-start write failure as a sticky `ObservabilitySink` health
issue and makes Runtime unready; later activity cannot reconstruct a missing
fact. Reaching the configured file limit is a normal bounded terminal, not a
sink failure. The Runtime process CPU/RSS monitor described below is now
implemented. Cross-restart metric aggregation and adapter-attributed
Provider/sandbox resource metrics remain incomplete.

### Resource measurement authority

Resource facts are split by what Runtime can actually observe:

- the Runtime process monitor samples only its own PID, using accumulated CPU
  milliseconds and current resident memory from a platform implementation;
- the first sample establishes a CPU baseline, subsequent monotonic deltas
  enter a fixed bounded histogram, and every valid RSS sample enters a byte
  histogram plus current/maximum gauges;
- the monitor is built in, starts and stops with Runtime, has no plugin or
  Agent-facing registration surface, and exposes a sticky health failure if
  sampling or its task terminates unexpectedly;
- Provider network bytes belong to the Runtime-owned HTTP body boundary;
  sandbox CPU, memory, and network belong to the concrete sandbox adapter and
  its cgroup/job/container accounting handle;
- machine-wide network-interface totals, command output sizes, token counts,
  and Agent-reported numbers are not network-resource evidence.

Every metric carries `observed`, `unavailable`, or `failed` semantics. An
unsupported or not-yet-instrumented authority is `unavailable`, not a zero
sample. This preserves an honest operational snapshot while adapters are
completed independently. The in-process snapshot is not yet a durable metric
store; cross-restart aggregation remains open.

Implementation status: Runtime synchronously establishes the current PID and
first RSS/CPU baseline before boot returns, then refreshes only that PID once
per second on an owned task. CPU deltas and RSS values use fixed exported
buckets; RSS also exposes current and maximum values. CPU counter regression,
missing process data, refresh failure, or blocking-task failure stops sampling,
preserves prior observations, emits a typed content-free failure fact, adds
`ResourceSampler` to health, and makes Runtime unready. Shutdown cancels and
joins the monitor. Network status remains explicitly `unavailable`.

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
- The official `sysinfo` 0.39.6 crate source and documentation define
  `Process::accumulated_cpu_time()` as cumulative CPU milliseconds and
  `Process::memory()` as current resident physical memory. Its CPU guidance
  requires two samples for a meaningful delta. Its network API is
  interface-wide, which is why Sylvander does not use it for Runtime or
  operation attribution.

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
