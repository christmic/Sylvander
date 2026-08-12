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

One closed Runtime storage facade owns sessions, messages, runs, turns, steps,
usage, artifacts, approvals, audit, registries, profiles, memory, and evidence.
Services request repository operations through one transaction rather than
opening SQLite independently. Schema namespaces may retain separate integrity
fingerprints, but backend lifecycle, connection, transaction, backup, and
health ownership are unified.

The initial backend is built-in SQLite. There is no storage plugin registry or
public backend trait.

## Built-in observability

Runtime assigns correlation identifiers and emits one typed internal lifecycle
event for admitted, started, retried, authorized, executed, persisted,
published, interrupted, and failed states. Built-in tracing, metrics, durable
evidence, and health views consume those facts. Observability sinks are not
runtime extensions at this stage.

A public success event requires both a committed storage outcome and a terminal
observability fact. Content is excluded by default and governed separately
when capture is enabled.

## Why this layer is above Agent

Agent can fail and retry without owning a durable Session. Runtime cannot
publish success, change ownership, or grant authority without durable and
auditable product rules. Keeping those responsibilities here prevents model
execution concerns from leaking into API, Channel, TUI, or desktop modules.
