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
Registry, profile, evidence, audit, Guardian, and artifact stores still open
through their existing Runtime-owned services; there is not yet a cross-domain
transaction or one backend health record. Those are remaining implementation
work, not capabilities callers may assume today.

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
counters, emits structured tracing, and exposes the counters through the
operational snapshot. Existing durable evidence remains a separate path;
durable observation, sink-failure health, resource histograms, and an atomic
storage/terminal-observation commit rule remain incomplete.

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
