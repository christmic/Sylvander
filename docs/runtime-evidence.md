# Runtime evidence and self-improvement boundary

Sylvander preserves structured runtime evidence so failures can be reproduced,
evaluated, and converted into reviewed improvements. This store complements
operational logs; it is not a transcript dump and it does not authorize an
Agent to edit or deploy itself.

## Data model

The SQLite evidence store normalizes six layers:

- **run** — one server process lifetime, including clean or interrupted end;
- **turn** — one user request in a session, with Agent identity, timing, sizes,
  status, and content digest;
- **step** — a tool invocation, including tool name, timing, sizes, digest, and
  success/failure status;
- **outcome** — a terminal completion or interruption attached to a turn;
- **event** — the append-only bus observation used to reconstruct ordering and
  diagnose normalization defects.
- **feedback** — an explicit positive/negative user assessment bound to a real
  run and optionally to a turn from that same run. It records bounded notes and
  corrections, task result, tags, artifact and validation references, privacy
  class, and Runtime-derived principal/channel/transport attribution.

Run, session, turn, step, bus-message, and tool-call identities provide
correlation without depending on log text. Query APIs return bounded turn
summaries with step/failure counts and outcome state; raw payloads are not part
of those summaries.

The cohort analysis API requires an explicit half-open time window and bounded
result limit. It selects turns in stable `(started_at, id)` order and returns a
SHA-256 digest over the selected structured facts. Reports expose:

- terminal success rate and a deterministic failure taxonomy;
- input/output tokens and cost only when every recorded iteration was priced;
- per-turn latency plus mean, p50, and p95 latency;
- tool calls/failures, approval requests/decisions, model retries, and
  interaction timeouts;
- positive/negative feedback coverage under an explicit privacy scope.

Warnings make mixed Agents, incomplete outcomes or pricing, sparse or
mixed-privacy feedback, excluded run-level feedback, and limit truncation
visible. The analyzer never reads prompt, response, correction, tool payload,
or other raw content.

## Evaluation registry

The evidence database also owns an immutable evaluation registry:

- scoring adapters have sequential revisions and a digest of the exact
  executable/configuration that produces their named metric;
- dataset revisions contain digest-pinned references, require both fixture and
  held-out cases, reference registered scorer revisions, and are stored in
  canonical case-ID order;
- baselines bind one exact dataset revision to named metric values, sample
  counts, score direction, and an allowed regression in basis points;
- candidate comparisons require the complete metric set and exact sample
  counts. Missing, extra, duplicate, or invented metrics fail instead of
  producing a partial pass.

Every definition has a deterministic SHA-256 digest. Re-registering the exact
definition is idempotent; changing an existing revision, skipping a revision,
or referencing an unknown component fails.

## Improvement proposals

An improvement proposal is an immutable, digest-addressed definition that
must name:

- the exact cohort digest and one or more digest-pinned evidence references;
- a bounded hypothesis and expected benefit;
- low, medium, or high risk plus the affected components;
- a concrete rollback plan;
- one or more registered dataset revisions and their matching baselines;
- the creating principal digest and timestamp.

Proposals begin as `draft` and advance with optimistic concurrency through
`ready_for_review`, `approved` or `rejected`, `experimenting`, and finally
`completed` or `rolled_back`. Every transition records the actor digest,
timestamp, and optional bounded reason. Invalid jumps and stale state revisions
fail. Approval authorizes only an isolated experiment; it does not itself
merge or deploy code.

## Capture policy

`server.evidence.content` selects one of three policies:

- `metadata_only` stores event types, timestamps, byte sizes, attachment
  counts, and SHA-256 digests. It does not store prompts, responses, tool input,
  or tool output.
- `redacted` additionally stores a structural JSON envelope whose payload is
  replaced with `[REDACTED]`.
- `full` stores the serialized bus message. It is opt-in and requires an
  operator-defined privacy, access, backup, and deletion policy.

Secrets must never be deliberately sent to the ledger. Digests are correlation
and integrity hints, not anonymization: low-entropy values can be guessed, so
the database remains access-controlled data under every policy.

## Lifecycle and recovery

The recorder subscribes before configured Agents start. On graceful shutdown
it drains queued messages, marks active turns interrupted, then closes the run.
When a database is reopened after a crash, any remaining running run, turn, or
step is marked `interrupted`. Evidence therefore never converts an unknown
result into success.

At startup the store deletes completed runs older than `retention_days`,
including their turns, steps, outcomes, events, and feedback. Active and crash-recovery
records are retained. Backup and legal-hold exceptions require a future
operator policy rather than silently overriding retention.

## Gated improvement flow

Evidence feeds a deliberately separated pipeline:

1. select a reproducible cohort with explicit privacy constraints;
2. classify failures and form an evidence-linked hypothesis;
3. register baseline and held-out evaluations;
4. create an improvement proposal with risk and rollback criteria;
5. implement only in an isolated Git worktree;
6. compare baseline and candidate results;
7. require human approval before merge or deployment;
8. observe production and roll back on declared thresholds.

Runtime evidence is input to this process, not an instruction channel. User
content cannot become a system prompt, skill, memory, source-code change, or
deployment merely because it appears in the ledger.

## Current boundary

The durable store, bus recorder, crash recovery, content policies, retention,
Rust query surface, and evidence-linked feedback API are implemented. Feedback
attribution is derived at the authenticated Runtime boundary rather than
accepted from the client, and references are bounded and digest-validated.
Deterministic privacy-aware cohort analysis and the versioned evaluation
registry and governed improvement proposals are implemented. Worktree
experiments, signed result bundles, merge approval, and deployment observation
remain the P5 backlog and must be completed before claiming autonomous
self-improvement.
