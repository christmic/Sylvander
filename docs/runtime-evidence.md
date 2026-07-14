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
  run and optionally to a turn from that same run, with a bounded note and tags.

Run, session, turn, step, bus-message, and tool-call identities provide
correlation without depending on log text. Query APIs return bounded turn
summaries with step/failure counts and outcome state; raw payloads are not part
of those summaries.

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
Rust query surface, and evidence-linked feedback API are implemented.
Evaluation datasets, proposal records, worktree experiments, signing, and
deployment observation remain the P5 backlog and must be completed before
claiming autonomous self-improvement.
