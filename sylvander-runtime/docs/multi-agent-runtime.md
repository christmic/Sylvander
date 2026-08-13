# First-class multi-Agent Runtime

This document is the current contract for multiple Agent instances inside one
Sylvander Session. It distinguishes durable mechanisms that are implemented
from topology shapes that are only representable today.

## Identity and Session ownership

A Session is not an Agent. It is the durable collaboration boundary containing
many `AgentInstance` participants. Each participant has:

- a globally stable instance ID and one immutable Agent definition revision;
- an origin (`Defined` or a fork receipt), role, and capability revision;
- an independent lifecycle revision and execution state;
- an independently revisioned, frozen effective execution configuration;
- an approval route and history view;
- an optional Agent-specific workspace view.

The reusable Agent definition and the concrete Agent instance are separate.
Forking creates a new first-class instance; it does not create an alias for the
parent run. Runtime authentication binds every coordination mutation to the
calling Session and Agent instance.

Exactly one participant has the root moderator role. Its lease epoch and
fencing token make moderator authority replaceable without allowing an old
moderator process to commit a late decision.

## History models

Runtime represents both collaboration primitives explicitly:

- `ForkSnapshot` freezes an exact parent history prefix in the same SQLite
  transaction that appends membership and topology. Later parent messages are
  invisible to the child.
- `SharedLane` identifies an Agent cursor over shared append-only Session
  history.

Production supports governed fork creation and separately defined Agent
revisions. Defined participants use a shared-lane view; dynamic lane cursor
operations remain internal.

## Governed topology and work

`SessionTopology` supports `ParentOf`, `Peer`, and `Reviews` edges and rejects:

- ownership cycles and disconnected ownership trees;
- duplicate or asymmetric peer facts;
- stale membership/topology revisions;
- unreachable message routes.

Fork and defined-Agent admission create the required `ParentOf` ownership edge.
Runtime also exposes governed, revision-CASed `Peer` and `Reviews` attachment;
dynamic callers cannot rewrite `ParentOf` and therefore cannot bypass the
moderator-rooted ownership tree.

Work is a durable DAG of bounded `CoordinationTask` records. Every task carries
an assignee, token budget, handoff ceiling, state, and optimistic revision.
Dependencies are cycle-checked before commit. Handoffs use a selected
arbitrator, task/topology revision fences, and an atomic assignment update.

## Loop and waste governance

Rules are authoritative; heuristics only raise evidence-bearing findings.
Runtime currently applies:

- hard limits for Agent count, ownership depth/fanout, active work, token
  budget, message attempts, and handoff count;
- Tarjan-style strongly connected component detection over wait-for edges;
- evidence stagnation detection over a bounded progress window;
- alternating-edge detection for handoff ping-pong;
- bounded mailbox leases, concurrency, retry, dead-letter, and coalesced wake
  scheduling.

Hard-stop findings cannot be overridden by a conditional continue verdict.
Heuristic findings may be continued only by the fenced moderator with explicit
conditions, rationale, and evidence references. That authorization is scoped
to the exact stable intent and is returned with the successful outcome.

## AI-native moderator arbitration

An arbitration case freezes the membership revision, topology revision,
moderator lease epoch, and fencing token that produced it. A decision supports:

- continue with durable conditions;
- replan selected tasks by blocking them;
- reassign eligible tasks within the handoff budget;
- suspend selected Agents into explicit reconciliation;
- cancel selected non-terminal tasks.

Decision validation, domain effects, decision persistence, and
`Open -> Applied` commit in one SQLite transaction. Exact retries return the
same applied case. A conflicting retry fails. Expired cases are revision-CASed
to `Expired` and followed by a deterministic, bounded renewal chain, so a
stable caller intent cannot become permanently wedged on an expired case.

The moderator is the final Session arbitrator, not an unrestricted bypass.
It cannot suspend itself, reference unknown facts, continue a hard stop, or
apply a verdict to a task/Agent state that changed after assessment.

## Communication and crash recovery

Messages are durable routed envelopes with stable IDs, hop ceilings, expiry,
delivery attempts, claim epochs, and terminal states. Before automatic
execution, Runtime atomically changes the envelope to `Delivered` and binds it
to one predetermined durable turn ID.

On boot and wake:

1. recover `Delivered` receipts before claiming new messages;
2. acknowledge an already completed turn without executing it again;
3. execute only when no durable turn exists;
4. escalate any non-completed durable turn as an uncertain hard stop;
5. wake the fenced moderator and leave the original message unacknowledged.

Execution failures are inspected in the same drain pass. A failure before the
turn becomes durable remains retryable; a failure after persistence escalates
immediately. This is the mailbox-level composition of persistent execution
position and independent replay policy.

Tool and model effects use the separate Session execution ledger. Prepared,
started, committed, and result-persisted boundaries are durable; replay safety
is not inferred from capability class. Workspace journal receipts and stable
invocation IDs decide resume/reconcile/manual outcomes.

Each `(Session, AgentInstance)` has one durable effective-configuration cell.
The moderator cell advances with Session configuration while a fork copies its
parent cell in the same membership/topology transaction. A defined Agent gets
an exact Runtime-resolved definition/model/permission cell. Every turn copies
that cell before effects, and boot fails closed if a live participant lacks or
mismatches it.

## Workspace concurrency

A writable fork is provisioned an Agent-specific isolated worktree by default.
Its durable workspace view records source, effective path, target/branch,
membership revision, lease/fencing data, and provisioning receipt. Boot
reconciliation retains only durable active leases.

Agents do not concurrently mutate the source checkout. A merge requires an
exact reviewed workspace revision and moderator approval; source advancement
turns the operation into a conflict rather than an implicit merge. Read-only
participants may use a shared view. This provides isolation and optimistic
synchronization; semantic multi-branch merge planning remains a higher-level
moderator task.

## Persistence choice

SQLite is the authoritative local Runtime backend because the correctness
boundary needs multi-table transactions, foreign keys, uniqueness, revision
CAS, crash-safe WAL behavior, and deterministic restart queries. JSONL is
appropriate for exported observation streams, not for membership + topology +
task + mailbox atomicity. A remote database can later implement the same store
contracts, but replacing SQLite with a distributed system does not remove the
need for these invariants.

Runtime uses exact latest-schema validation and fails closed on unknown,
partial, future, or damaged schemas. Coordination shares the Session database
so membership, history fork receipts, task state, and mailbox turn receipts can
commit atomically.

## Observable facts

The typed Runtime observation bus, bounded debug JSONL, and health snapshot
expose low-cardinality counters for participant activation, topology updates,
enqueue, arbitration required, moderator authorization/rejection, applied
decisions, and mailbox escalation. Durable
cases, decisions, message states, turn receipts, task revisions, lifecycle
revisions, and workspace views provide the audit trail. Prompt and message
payload content is excluded from Runtime lifecycle metrics.

## Remaining product work

The durable substrate is complete for heterogeneous defined Agents, governed
fork workers, dynamic collaboration edges, and mailbox execution. The
following remain explicit follow-on work, not implied by the current types:

- a packaged swarm coordinator API over the existing coordinator role,
  membership, task, relation, and mailbox primitives;
- task execution leases that stop an already-running assignee at a replan or
  reassignment boundary;
- semantic merge planning across multiple isolated worktrees;
- operator/API projections for listing cases, decisions, topology, tasks, and
  mailbox recovery state.

The orthogonal boundary between first-class Agents, internal model roles,
Agent-authored workflow, multimodal perception, soft recovery, Doctor, and the
Runtime benchmark matrix is specified in
[`agent-cognition-workflow-doctor.md`](agent-cognition-workflow-doctor.md).
