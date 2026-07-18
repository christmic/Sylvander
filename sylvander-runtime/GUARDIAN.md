# Worker / Guardian Runtime

This document is the module-owned design and operator contract for Worker
capability isolation and durable Guardian curation. The implementation lives in
`src/capability_runtime.rs`, `src/guardian_curation.rs`, and
`src/guardian_runtime.rs`; its test bodies live under `tests/unit/`.

## Boundary

A Worker is the conversational actor for one Runtime-derived user, Agent,
session, and workspace scope. A Guardian is a non-conversational service actor.
It receives immutable event references and curates governed memory changes.
They do not share a registry, identity, or ambient capabilities.

The boundary has two independent enforcement points:

1. `ActorCapabilityRuntime` freezes a per-run registry. Discovery can return
   only definitions in that actor's immutable snapshot.
2. `ActorCapabilitySnapshot::invoke` re-authorizes the concrete call, rejects
   caller-supplied owner selectors, and durably records an authorization event
   before a handler can run.

A snapshot is therefore a visibility boundary, not a bearer credential.
Unknown and hidden capability names return the same content-safe result.

Production tool execution adds one outer `ToolInvocationGateway`. Runtime
builds it from the exact Agent tool descriptors, freezes that executable
surface for the turn, and maps each route to the actor policy class. Built-in
read/write/terminal/control/extension routes, MCP, browser, host control, and
memory candidates all pass this gateway immediately before execution and
report exactly one terminal outcome. Skills enter the same content-addressed
turn snapshot as `PromptContext`; they cannot become executable merely by
appearing in a prompt. Approval remains a typed pre-execution gate, and large
results are persisted through the bounded evidence artifact sink rather than
being copied unbounded into audit or transcript data.

## Actor capabilities

Worker and Guardian registries are built separately. A name may not exist in
both registries, because aliasing would make route provenance ambiguous.

Worker policy permits ordinary session/relationship/candidate operations and
interactive capabilities. It denies user-profile, canonical-memory, and
workspace-knowledge mutation.

Guardian policy permits governed data reads and mutation handlers. Registry
construction rejects terminal, browser, host-control, arbitrary MCP,
session-append, relationship-append, and Worker candidate-append classes.
The Runtime still supplies an explicit Guardian allowlist; a class being
eligible does not automatically register it.

`RuntimeOwnerScope` is constructed from authenticated Runtime context.
Capability input is recursively rejected if it attempts to select `owner`,
`user_id`, `agent_id`, `session_id`, or `workspace_id`. Authorized handlers see
the Runtime scope only after the second policy check.

## Guardian service identity

The Guardian identity has:

- a stable service ID;
- a credential revision;
- an absolute expiry.

The identity value has private fields and is issued by Runtime. Every snapshot,
run lease, and mutation delivery validates it. Audit stores only a domain-
separated service digest. Credential rotation creates new Runtime service
objects; the stable service digest allows a new credential revision to recover
the same durable run without copying the secret or revision into an event.

## Durable curation store

The curation database is independent from conversation and session databases.
It uses an exact latest-only SQLite schema with a dedicated application ID. An
empty file is initialized; an old, unknown, or foreign schema fails closed.
There is no compatibility fallback or in-memory production backend.

The schema contains:

- `guardian_outbox`: idempotent immutable source-reference events;
- `curator_runs`: one durable leased run per event;
- `memory_candidates`: typed candidate head and state;
- `guardian_policy_decisions`: immutable deterministic decisions;
- `guardian_mutation_outbox`: idempotent authorized store mutations;
- `guardian_curation_audit`: content-safe state transition audit;
- `capability_invocation_audit`: pre-execution and terminal capability audit.

SQLite write operations use immediate transactions. Busy timeout is bounded.
Run and mutation claims carry random tokens and finite leases. A stale token,
expired lease, wrong run, wrong candidate owner, wrong revision, or wrong state
fails without exposing another owner's record.

## Event and run lifecycle

```text
Runtime event reference
  -> guardian_outbox.pending
  -> curator_runs.running (leased)
  -> candidate extraction / curation
  -> curator_runs.succeeded
  -> guardian_outbox.completed
```

`event_id` is the outbox idempotency key. Re-enqueueing the same event is a
no-op only when all immutable fields match; a different payload under the same
ID is an idempotency conflict.

One event has one `CuratorRun`. A crash leaves a finite lease. Reclaim increments
the attempt while retaining the run ID, curator version, and policy revision.
Changing either version while resuming a run fails closed. Transient failure
makes a run retryable at a bounded future time. Irrecoverable failure preserves
all candidates and audit rows while making the event terminal.

## Candidate state machine

```text
Extracted
  -> Classified
  -> Duplicate
  -> Conflict -> Rejected | AwaitingConfirmation | PolicyPending
  -> AwaitingConfirmation -> Rejected | PolicyPending
  -> PolicyPending -> Rejected | Authorized
  -> Authorized -> CommitPending
  -> Committed
       -> correction PolicyPending -> Corrected
       -> decay PolicyPending      -> Decayed
       -> forget PolicyPending     -> Forgotten
  -> DeliveryFailed
```

Extraction uses a stable `(run_id, source_key)` pair. Replaying the same pair
returns the existing candidate only if its content digest and evidence match.

Classification records:

- scope and Runtime-derived owner;
- evidence references;
- confidence in basis points;
- explicit or inferred origin;
- sensitivity;
- deterministic consent state;
- finite retention and expiry;
- deduplication key and optional conflict reference.

Relationship and user-profile candidates require a Runtime user. Workspace
knowledge requires a workspace ID present in the event's Runtime-owned
workspace set. Duplicate/conflict references must have exactly the same owner
and scope; otherwise the result is a content-safe access denial.

## Confirmation and deterministic policy

User-profile, personal, and secret candidates enter
`AwaitingConfirmation`. Confirmation is a separate transition; classifier
output cannot set it.

Every commit, correction, decay, and forget action goes through
`DeterministicGuardianPolicy` at a fixed revision. An allow decision is tied to
the exact candidate revision and action. Scheduling a mutation without that
matching allow row fails closed.

Current deterministic rules include:

- evidence and finite retention are mandatory;
- denied consent is terminal;
- profile mutation requires a user and explicit confirmation;
- personal relationship memory requires confirmation;
- secret material cannot be stored;
- personal/secret content cannot enter cross-user canonical Agent memory;
- inferred canonical memory requires at least 80% confidence;
- workspace knowledge requires an authorized workspace and rejects personal
  material;
- forgetting an existing governed record is always permitted after ownership
  and evidence checks.

Semantic model classification informs the candidate, but is never the
authorization decision.

`do_not_learn` is another Runtime-owned policy input. The Worker gateway denies
memory-candidate tools for an opted-out owner. Guardian admission rejects new
events, drain-time revalidation prevents already-queued events from producing
candidates, and mutation delivery revalidates new commit actions. Preference
store failure is a denial. Explicit correction, export, deletion, decay, and
forget operations remain available because they govern existing data rather
than create a new learned fact.

### Public confirmation boundary

Interactive clients use the negotiated `memory_confirmation_v1` capability.
The latest-only protocol has two operations:

- `List { session_id }` returns bounded, sanitized pending summaries for that
  exact session; and
- `Decide { session_id, candidate_id, expected_revision, confirm|reject }`
  records one explicit decision.

Neither operation carries a user, Agent, workspace, or owner selector.
`UiService` authorizes the authenticated boundary, loads the persisted owned
session, and derives the Guardian owner from that state. A pending candidate
is returned only when both its owner and immutable `origin_session_id` match.
The candidate revision is an optimistic-concurrency token: stale, replayed,
cross-owner, cross-session, unknown, and already-resolved decisions fail with
the same bounded forbidden/conflict classes instead of exposing candidate
existence.

Runtime drains the relevant Guardian pass before listing and after recording a
decision, so a just-finished turn can surface its confirmation without waiting
for a background poll. Confirmation remains a policy transition, not a direct
store write: a confirmed candidate must still pass deterministic policy and
idempotent mutation delivery. Rejecting, pressing escape, losing the client,
or omitting a response never implies consent.

## Mutation delivery

Policy authorization creates an outbox mutation with a deterministic
idempotency key derived from candidate ID, candidate revision, action, and
policy revision. The owning canonical-memory/profile adapter must apply this
key idempotently.

Delivery uses another finite lease. If the sink succeeds, the caller
acknowledges the mutation and the candidate becomes `Committed`, `Corrected`,
`Decayed`, or `Forgotten` in the same SQLite transaction. A retryable sink
failure preserves the key and schedules bounded redelivery. A permanent
failure marks the mutation `dead_letter` and candidate `DeliveryFailed`.

Commit/correction payloads include content. Decay/forget payloads contain only
the record identity, digest, retention, and action; they never repeat content
that should be removed.

The sink success / acknowledgement crash window is resolved by the sink's
idempotency key: after lease recovery, reapplying the same mutation must return
the same successful outcome.

## Audit and privacy

Capability audit stores actor, capability and policy revisions, owner digest,
phase, and outcome. It never stores input, output, owner IDs, schema, or
credentials. If the pre-execution record is unavailable, invocation fails
before the handler runs. If only the terminal record fails after execution,
the result is explicitly `ExecutionOutcomeUncertain`; callers must inspect the
pre-execution invocation ID and must not blindly replay a side effect.

Curation audit stores IDs/references, state transitions, fixed reason codes,
and optional content digests. Error codes are restricted to lowercase
content-safe tokens. Raw transcript text and model prompts are not copied into
either audit table.

## Runtime integration

`GuardianRuntime` is the only composition owner. It:

1. issues a short-lived Guardian identity and builds disjoint Worker and
   Guardian registries;
2. opens one durable `GuardianCurationStore` and uses it as the capability
   audit sink;
3. constructs immutable Worker snapshots from authenticated `SessionContext`;
4. constructs immutable Guardian snapshots from Runtime-derived owner scope;
5. accepts session-close, candidate, feedback, confirmation, and retention
   events only as immutable references;
6. supervises run claims, bounded retries, mutation delivery, and finalization;
7. applies Agent-canonical mutations to a separate latest-schema SQLite store
   in the same transaction as its idempotency receipt;
8. rotates credentials by reopening the same stores with an incremented
   credential revision and stable service ID; and
9. drains the active pass during graceful Runtime shutdown.

The built-in deterministic curator materializes one content-safe canonical
record for each Runtime event reference. It never copies transcript or feedback
content. The record is not marked committed until the canonical sink has
actually applied the mutation. Unsupported target scopes are dead-lettered
rather than acknowledged.

The supervisor intentionally has no in-memory production fallback. Restarting
with the same curation and canonical paths recovers expired run and mutation
leases. The canonical sink treats `(idempotency_key, mutation_id, body_digest)`
as immutable: exact replay succeeds without a second write, while a conflicting
replay fails permanently.

Configured Runtime boot opens `guardian-curation.db` and
`guardian-canonical.db` beneath the resolved Runtime data directory, audits
Worker bindings for restored sessions, and starts bounded background catch-up.
Persisted feedback is enqueued by its evidence-ledger ID and digest. Every
session closure is enqueued only after durable deletion succeeds; coding
worktree cleanup remains part of that Runtime-owned lifecycle. Runtime shutdown
stops new channel work first, despawns Agent runs, then waits for the active
Guardian pass before closing evidence and maintenance services.

No channel, model tool argument, MCP server, or plugin may construct an owner
scope or Guardian identity.

## Recovery checklist

After an unclean restart:

1. open the database; reject any schema mismatch;
2. wait until each previous lease expires;
3. reclaim available runs using the same curator and policy versions;
4. resume candidates from their persisted state and expected revision;
5. reclaim mutation deliveries and apply the same idempotency key;
6. inspect `DeliveryFailed` and failed runs before operator retry/correction;
7. finalize a run only when no active candidate or pending/claimed mutation
   remains.

## Verification

Focused tests cover hidden discovery, forged owner selectors, cross-owner
candidate references, expired identities and leases, audit failure, idempotent
event/extraction/run/mutation replay, confirmation, policy denial, conflict,
retry, restart with credential rotation, correction, decay, forgetting,
exact-schema rejection, durable audit, real canonical delivery, and exact
idempotency replay.

When the modules are linked into `sylvander-runtime`, run:

```sh
CARGO_TARGET_DIR=target/guardian \
  cargo test -p sylvander-runtime capability_runtime
CARGO_TARGET_DIR=target/guardian \
  cargo test -p sylvander-runtime guardian_curation
CARGO_TARGET_DIR=target/guardian \
  cargo test -p sylvander-runtime guardian_runtime

CARGO_TARGET_DIR=target/guardian \
  cargo clippy -p sylvander-runtime --all-targets -- -D warnings
```
