# Crash-safe Agent execution recovery

Status: normative target; implementation gates below must not be advertised as
complete until their executable verification gates pass.

This document defines how Sylvander decides whether an interrupted tool call
may continue, reconcile, retry, stop, or require an operator. It extends the
ordinary tool execution boundary without making authorization class stand in
for recovery safety.

## Safety objective

After process loss at any persisted execution boundary, Runtime must choose one
deterministic action from durable facts. It must never infer an external effect
from a `Running` row, logs, public events, or absence of a terminal audit.

The primary invariant is:

> Runtime never starts an effect twice unless the frozen tool contract permits
> same-identity replay and the concrete adapter attests that contract.

The secondary invariants are:

1. An invocation has one stable Runtime identity across authorization,
   execution, audit, reconciliation, and replay.
2. Execution positions advance monotonically by compare-and-swap (CAS).
3. A durable result is committed before another model iteration can consume it.
4. A committed effect is never re-executed merely to reconstruct its result.
5. Unknown tools, changed contracts, missing adapters, corrupt receipts, and
   ambiguous effects fail to manual reconciliation.
6. Capability audit and Evidence are content-free projections of recovery
   truth; neither is the recovery source of truth.
7. Recovery records do not expose model input, tool output, credentials,
   commands, paths, or remote receipts through logs or public API.

## Ownership

| Layer | Owns | Must not own |
|---|---|---|
| Agent | Permission-independent recovery vocabulary, frozen tool declaration, legal continuation input | Databases, boot scans, concrete journals, public DTOs |
| Runtime Session storage | Invocation identity, monotonic execution ledger, encrypted or governed payload references, recovery leases | Tool-specific external reconciliation logic |
| Runtime execution adapter | Honest support attestation, same-ID invocation, receipt or journal reconciliation | Generic continuation policy |
| Runtime recovery coordinator | Boot classification, per-invocation decision, bounded leases, turn continuation admission | A second copy of the ledger |
| API | Redacted status, reason, timestamps, and allowed operator decisions | Executable handles, raw receipts, model/tool content |
| Observability/Evidence | Counters and terminal decision projections | Recovery authority |

## Independent tool contract

Authorization and recovery are orthogonal. `ToolInvocationClass` continues to
answer which authority a tool can exercise. Every `ToolSpec` additionally
freezes one `ToolRecoveryPolicy`:

```rust
enum ToolRecoveryPolicy {
    NeverReplay,
    RetryWithSameInvocation,
    ReconcileBeforeRetry,
}
```

- `NeverReplay`: once the effect might have started, automated replay is
  forbidden.
- `RetryWithSameInvocation`: the adapter accepts the same stable invocation
  identity and guarantees duplicate delivery does not duplicate the effect.
- `ReconcileBeforeRetry`: the adapter must first resolve a durable receipt or
  journal and may retry only when reconciliation proves that no effect exists.

Constructors default to `NeverReplay`. A dynamic or external source such as
MCP remains `NeverReplay` until both its immutable declaration and its Runtime
adapter explicitly support a stronger policy. Read authority never implies
retry safety, and mutation authority never by itself forbids recovery.

Runtime computes the effective policy as the conservative intersection of the
frozen Agent declaration and adapter attestation. There is no permissive
fallback:

```text
effective = min_safety(tool_declaration, adapter_attestation)
missing declaration or attestation = NeverReplay
```

The policy participates in tool-surface hashing. A policy change therefore
creates a different immutable capability revision and cannot reuse an older
approval or in-flight invocation.

## Durable execution ledger

Terminal outcome and execution position are separate dimensions. The first
implementation uses these effect positions:

```rust
enum ToolExecutionPosition {
    Prepared,
    Authorized,
    EffectStarted,
    EffectCommitted,
    ResultPersisted,
}
```

`Prepared` is inserted before an approval or adapter call. `Authorized` means
approval and pre-execution audit are durable but no effect call has started.
`EffectStarted` is persisted before crossing the adapter effect boundary.
`EffectCommitted` requires adapter evidence that the external effect exists.
`ResultPersisted` means the exact model-visible observation is durable.

Each invocation ledger row contains:

- Runtime-generated `invocation_id`, plus `(session_id, turn_id, call_id)` as a
  unique correlation key;
- tool route, invocation class, frozen recovery policy and capability revision;
- canonical prepared-input digest, never raw input in the ledger;
- position, terminal state, monotonic revision, and timestamps;
- governed opaque references for recovery receipt and model-visible result;
- recovery decision, content-free reason, attempt count, and lease owner/expiry.

The stable invocation identity is generated once at `Prepared`, persisted in
the same transaction, and supplied unchanged to authorization and the adapter.
A repeated `(session, turn, call)` insert succeeds only when every immutable
fingerprint matches; a mismatch is corruption and fails closed.

Position transitions use one storage operation with expected revision and
expected current position. Legal forward transitions are:

```text
Prepared -> Authorized -> EffectStarted -> EffectCommitted -> ResultPersisted
Prepared/Authorized -> terminal Rejected
any non-terminal position -> terminal Failed or Abandoned
EffectStarted -> terminal ManualReconciliationRequired
```

No update may move backward, skip a required durable boundary, replace an
immutable field, or overwrite a terminal. Idempotent repetition of the exact
same transition returns the existing row; conflicting repetition fails.

## Crash classification algorithm

Runtime performs recovery before a restored Session can accept another turn.
It scans non-terminal turns, acquires a bounded per-turn recovery lease, then
classifies tool invocations independently from their last durable position and
effective policy.

| Last durable position | Effective policy | Decision |
|---|---|---|
| `Prepared` | any | Re-enter approval/preparation from frozen iteration |
| `Authorized` | any | Start effect using the stored invocation identity |
| `EffectStarted` | `NeverReplay` | Stop; require manual reconciliation |
| `EffectStarted` | `RetryWithSameInvocation` | Replay with the same identity |
| `EffectStarted` | `ReconcileBeforeRetry` | Reconcile receipt/journal first |
| `EffectCommitted` | any | Recover/persist result; never execute again |
| `ResultPersisted` | any | Continue the next Agent iteration |

`ReconcileBeforeRetry` produces exactly one of:

- `Committed(receipt, result)`: advance without re-execution;
- `NotCommitted`: retry once with the same identity;
- `RolledBack`: restart only if the adapter contract permits it;
- `Unknown`: require manual reconciliation.

Adapter errors, timeouts, malformed receipts, or conflicting journal state map
to `Unknown`, not `NotCommitted`. For a batch of parallel calls, the turn may
continue only after every call has a durable result or a durable terminal that
is legal as a model observation. One uncertain call blocks the whole turn.

Recovery leases use a compare-and-swap owner and expiry. Expiry permits another
Runtime instance to resume classification; it never authorizes an additional
effect. All decisions are recomputed from the newest row after lease acquisition.

## Durable Agent continuation

Tool recovery alone is insufficient. Before the first effect in one model
iteration, Runtime durably stores the provider-neutral assistant response and
ordered prepared tool calls. After each tool terminal, it stores the exact
provider-neutral tool observation before it is appended to conversation state.

The durable iteration record contains only Agent-owned neutral values and
governed payload references. Runtime reconstructs an `AgentContinuation` from:

1. the turn's immutable request/config/capability revisions;
2. its last durable provider-neutral model response;
3. ordered tool ledger rows and persisted observations;
4. iteration and compression counters.

Agent validates the continuation against its `TurnMachine` transition rules.
Runtime must not synthesize a phase or ask the provider to regenerate an
already persisted response. A missing or conflicting observation makes the
turn non-resumable and operator-visible; it does not trigger blind replay.

## First concrete adapters

The workspace mutation journal is the first reconciliation adapter. Its
prepared/applied/rolled-back/abandoned manifest is bound to `invocation_id`.
Recovery compares the governed manifest and filesystem evidence:

- applied and matching after-image -> `Committed`;
- prepared and matching before-image -> `NotCommitted`;
- rolled back and matching before-image -> `RolledBack`;
- missing, conflicting, or unverifiable bytes -> `Unknown`.

Command, Git, arbitrary MCP, browser, host-control, and extension tools remain
`NeverReplay` until an adapter proves a stronger contract. Pure built-in reads
may use `RetryWithSameInvocation` only after tests prove no consumptive or
external side effect. Memory mutation requires its existing idempotency key to
be bound to `invocation_id` before receiving a stronger policy.

## Public status and observability

The API exposes a redacted recovery summary with session/turn/call correlation,
position, decision, reason code, attempt count, first-seen/updated timestamps,
and whether operator action is required. Manual actions are explicit typed
commands with optimistic ledger revision; no generic “retry” command exists.

Runtime records content-free counters and structured events for:

- interrupted invocations discovered by position and policy;
- decisions: resume, same-ID retry, reconciled, blocked, or manual;
- reconciliation latency/outcome and recovery lease contention;
- CAS conflict, immutable fingerprint mismatch, and corrupt payload reference;
- resumed turn completion or terminal failure.

Logs contain correlation IDs and enums only. Receipt, journal bytes, input
digests, model output, and tool output are excluded. A public terminal is
emitted only after the corresponding ledger terminal and observability fact are
durable/recorded in the existing terminal ordering.

## Delivery and verification gates

1. Contract: recovery policy is frozen, hashed, authorization-checked, and
   defaults to `NeverReplay`; authority/recovery independence tests pass.
2. Ledger: exact-schema migration, CAS transitions, stable identity, boot scan,
   and safe classification pass without executing a tool.
3. Reconciliation: workspace journal and governed result/receipt persistence
   pass crash tests at every effect boundary.
4. Continuation: durable model iterations and tool observations reconstruct a
   valid Agent continuation and resume without regenerating committed work.

Deterministic fault injection must stop execution immediately after every
durable boundary. Tests assert effect count at most one, stable identity across
restart, no continuation past uncertainty, independent mixed-policy parallel
calls, lease takeover, redacted telemetry, and idempotent repeated recovery.
Each gate also requires formatting, strict all-target Clippy, tests,
warning-denied Rustdoc, and the module-scope import scan.
