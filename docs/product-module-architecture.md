# Product module architecture

This document defines the target layering for Sylvander as a complete product:
model protocols, Agent execution, Runtime services, storage, observability,
service protocols, and presentation clients. It complements the narrower
Agent/Runtime/API boundary in
[`agent-runtime-api-boundaries.md`](agent-runtime-api-boundaries.md).

## Layer map

```text
Presentation
  sylvander-tui / desktop client
        |
Service edge
  sylvander-server / Channel adapters
        |
Public contract
  sylvander-api
        |
Application Runtime
  Session service / Agent supervisor / execution service
  storage / observability / authorization
        |
Agent kernel                         Infrastructure adapters
  sylvander-agent                    SQLite / OCI / SSH / MCP
        |                                   |
Model contract                              |
  sylvander-llm-core <--------------- provider adapters
```

Dependencies point downward. Runtime is the application composition root and
the only layer allowed to join client identity, durable state, an Agent turn,
provider credentials, and infrastructure.

## Model foundation

`sylvander-llm-core` owns provider-neutral model identity, capability,
request/response, streaming, tool-schema, reasoning, and usage contracts. It
does not know Agent sessions, tools, UI, credentials, HTTP clients, or vendor
wire shapes.

Each provider crate owns exactly one official wire adapter and HTTP client.
Provider crates translate at their boundary and implement `ModelProvider`.
They do not depend on Agent or the client API.

## Agent kernel

`sylvander-agent` owns one bounded reasoning execution. It consumes an
immutable `AgentTurnRequest`, emits internal `AgentEvent` values, and returns an
`AgentOutcome`. The input includes a model-visible `ConversationSnapshot` and a
trusted `AgentExecutionContext`; neither is a product Session record.

The Agent owns policy declarations and ports, not infrastructure:

- tools declare schemas, coordination, filesystem, network, process, and
  sandbox requirements;
- the Agent asks an injected authorization port for a decision;
- the Agent asks an injected execution port to perform structured workspace or
  process work;
- mutation tools use an injected two-phase journal port, while Runtime owns
  snapshot layout, crash recovery, conflict detection, and rollback commands;
- compression uses an artifact persistence port, while Runtime owns the
  explicit storage root, identifier validation, retention, and cleanup policy;
- the Agent never chooses a host path, container image, SSH endpoint, database,
  credential, or network bypass.

## Runtime application layer

Runtime is the sole producer of redacted UI snapshots. See
[`runtime-ui-snapshots.md`](runtime-ui-snapshots.md) for model/platform/request
limits and archive-aware Session discovery. Channels serialize those values;
they do not assemble parallel Runtime truth.

Runtime owns four cohesive internal services.

### Session service

The Session service owns authenticated product Sessions, transcript and
configuration revisions, turn admission, persistence, resume, archive,
interrupt, and subscription. It converts API `TurnRequest` values into
`AgentTurnRequest` values and commits `AgentOutcome` atomically.

The Rust-only `MessageBus` port and bounded in-process adapter live beside the
Channel host contract in `sylvander-channel`. They carry Protocol DTOs but are
not themselves wire protocol. Runtime owns their composition and lifecycle;
Channels receive only the host and subscription capabilities they need.

### Agent supervisor

The supervisor owns per-Session serialization, cancellation, bounded
background tasks, exact Agent/model revisions, restart restoration, and
terminal state. An Agent execution is replaceable computation; Runtime's
durable Session is the product record.

### Execution service

The execution service implements the Agent's workspace and process ports. It
maps logical workspace and target identifiers to concrete adapters, validates
the prepared policy, starts the operation, collects bounded output and
artifacts, and records the terminal execution fact. Host-local execution,
workspace mutation manifests, rollback recovery, and filesystem-backed tool
result artifacts are concrete Runtime adapters rather than Agent facilities.

Implementation status: Runtime composes one immutable exact-target service and
shares it across every Agent revision. The named built-in `local` target and
configured SSH/OCI targets are resolved only by exact ID; missing IDs fail
closed. Runtime exposes redacted adapter kind, static isolation truth, and an
honest `ready/unverified/degraded` status. One Runtime-owned worker performs
concurrent five-second SSH/OCI probes every 30 seconds through the exact
configured adapter route, stores only a success bit and failure count, affects
readiness on degradation, and joins on shutdown. Per-operation latency and
resource observations remain follow-up work.

### Authorization service

The authorization service derives actor authority from authenticated Runtime
state, applies approval policy, issues one invocation grant, and records its
terminal outcome. Model input never enters authority fields.

## Sandbox model

The default architecture is **Agent uses a sandbox**, not **Agent runs inside
the sandbox**.

The Agent kernel and Runtime supervisor are trusted control-plane code. They
remain outside the tool sandbox so provider credentials, Session storage, and
authorization state are not mounted into untrusted execution. Every model-
triggered process is a data-plane operation and must pass through Runtime's
execution service.

```text
trusted process
  Runtime -> Agent -> PreparedToolCall -> authorization
                                  |
                                  v
                         execution service
                                  |
                                  v
                     disposable sandbox process
```

Structured file reads and writes use a bounded workspace adapter and do not
claim process sandboxing. Commands, Git subprocesses, executable hooks, and MCP
servers require an enforced process environment according to their declared
policy.

An enforcing sandbox implementation contains all of the following:

- a process launcher and process-tree cancellation boundary;
- filesystem mount/read/write policy derived from a logical workspace;
- denied-by-default network isolation or an approved, enforced egress proxy;
- memory, CPU, PID, wall-clock, output, and artifact limits;
- environment and credential filtering;
- capability/syscall/privilege restrictions appropriate to the platform;
- disposable lifecycle and deterministic cleanup;
- violation, resource, exit, and artifact observation.

The current concrete implementation is disposable OCI execution with an
explicit workspace bind, `--network=none`, read-only root, private temporary
storage, no new privileges, dropped capabilities, and resource ceilings.
Local and SSH adapters do not claim sandbox enforcement and therefore cannot
run prepared process tools through the Agent path.

Running an entire Agent worker inside a container may later be offered as
deployment defense-in-depth or multi-tenant isolation. It does not replace
per-tool authorization or the execution sandbox, because the worker still
holds broader model, storage, and control-plane authority than one tool call.

## Unified storage backend

This section defines the target contract. Runtime owns one configured storage
backend and one internal storage facade.
Product services do not open independent databases or call a concrete driver
directly.

```text
RuntimeStorage
  transaction()
  sessions()
  messages()
  runs_turns_steps()
  usage()
  artifacts()
  approvals_and_audit()
  registries()
  profiles_and_memory()
  evidence()
```

The facade provides one transaction boundary for operations such as:

1. admit a turn and append the user message;
2. record run/turn/step lifecycle and token usage;
3. store the assistant outcome and artifact references;
4. advance the Session revision and publish a durable terminal fact.

Large artifact bytes may use a backend-owned blob area, but their ownership,
digest, retention, and references commit through the same facade. A successful
client completion cannot be published before its durable transaction commits.

The initial implementation is a closed, Runtime-owned SQLite backend. Storage
traits and repositories remain `pub(crate)` and the configuration uses a
closed backend enum; there is no plugin ABI, dynamic backend registration, or
third-party extension point yet. Schema namespaces may remain separately
fingerprinted for integrity, but connection, transaction, lifecycle, backup,
and health ownership are unified.

Implementation status: `RuntimeStorage` currently owns the boot-selected
Session and relationship-memory repositories and `Runtime` exposes neither
backend publicly. Session schema v2 promotes turns to durable lifecycle
records. `begin_turn` commits the user message, immutable effective config,
and `running` state together; `complete_turn` commits the assistant message
and `completed` terminal together. Failed and interrupted terminals are also
persisted before their public event when a durable turn exists.

The same facade now emits a unified, content-free health snapshot for Session,
relationship memory, the Agent Registry, User Profiles, Evidence, the
encrypted turn-artifact service, credential-operation audit ledger, Guardian
curation, and Guardian canonical memory. Production composition retains concrete probe handles while
Agent revisions receive only their provider-neutral ports. Session health
rechecks the exact live schema, SQLite pages, and owned foreign keys;
relationship-memory health rechecks its exact schema, SQLite pages, and, when
configured, the independent authenticated anchor. Registry health rechecks its
exact shared namespace, current schema ledger, owned foreign keys, and SQLite
pages. User Profile health rechecks its exact schema and SQLite pages. Evidence
rechecks its exact base schema, database and foreign-key integrity, plus the
governance table shape when governed capture is active. Credential audit
rechecks its exact schema and database integrity. Both Guardian stores reject
any non-current object set on open and recheck their exact schema and SQLite
pages during health; curation additionally checks foreign keys. A health-only
`GuardianStorageProbe` carries cloned store handles without granting supervisor
control, credential rotation, or mutation authority. `Ready` means that live
probe succeeded, `Degraded` makes Runtime unready, and `Unverified` means the
component is intentionally absent (such as disabled artifact governance) or an
isolated composition has no concrete probe. A failed Session
count no longer makes the health endpoint fail before it can report Storage as
degraded. Paths, database errors, row data, and anchor material never enter the
snapshot.

Turn artifacts are location-neutral, encrypted, scoped, and independently
represented in unified health. The retrieval boundary is Runtime-owned: a
client supplies only an opaque locator, owning Session, and byte offset;
Runtime derives user identity, verifies Session ownership and artifact
provenance, performs an audited governed-store read, and returns at most 48
KiB of plaintext as Base64. Cross-user, cross-Session, expired, and deleted
records are uniformly not visible. Channels never receive storage handles and
Agent never receives a read authority. Cross-repository transactions and a
unified backup lifecycle remain incomplete. See
`sylvander-runtime/docs/application-services.md` for the exact contract.

## Built-in observability

This section defines the target contract. Runtime observability is mandatory
and not currently extensible. Every
operation carries stable `run_id`, `turn_id`, `step_id`, `tool_call_id`, and
trace correlation assigned by trusted Runtime code.

One typed internal `RuntimeEvent` stream feeds built-in sinks:

- structured tracing and redacted logs;
- counters, durations, resource use, queue depth, retry, and failure metrics;
- durable run/turn/step, authorization, artifact, and terminal evidence;
- health snapshots for storage, providers, Channels, sandboxes, MCP, and
  background workers.

The recorder is composed directly by Runtime rather than discovered through a
plugin registry. Content is excluded by default; captured content follows the
configured privacy, encryption, retention, and user opt-out policy. Sink
failure becomes visible Runtime health state and cannot be silently treated as
a successfully observed operation.

Implementation status: the closed typed `RuntimeEvent` recorder now covers
authorized chat admission, message-bus dispatch, turn and tool terminals,
model retries, and Session persistence outcomes. Runtime shares one recorder
with every Agent revision, feeding content-safe counters, structured tracing,
active-work gauges, bounded lifecycle latency histograms, unmatched-terminal
diagnostics, and the operational snapshot. The Session turn record is the authoritative
durable terminal; Runtime records the matching in-process terminal and only
then publishes the public terminal. Evidence recording remains a separate,
asynchronous governance projection of bus traffic and is never used to decide
whether a turn committed. The optional bounded debug projection makes a
post-start write failure a sticky `ObservabilitySink` health issue;
normal size-limit truncation is not a failure. Runtime process CPU/RSS
sampling is implemented as described below. Cross-restart metric aggregation
and adapter-attributed Provider/sandbox resource metrics remain incomplete.

Resource observation has two non-interchangeable authorities. A closed
Runtime process sampler owns periodic CPU and resident-memory facts for the
server process. Runtime initializes the sampler before publishing readiness,
uses cumulative CPU time only to derive interval deltas, records RSS as an
instantaneous gauge/distribution, and owns cancellation plus join during
shutdown. A missing process, failed refresh, or terminated sampler is a sticky,
content-free health issue. The first CPU refresh is only a baseline and never
enters a histogram.

Per-operation network bytes and sandbox resource consumption must instead be
reported by the Runtime-owned adapter that possesses the actual HTTP stream or
sandbox accounting handle. Host network-interface totals are forbidden as a
substitute because they include unrelated processes and cannot be attributed
to a Runtime, Agent, Session, turn, or tool call. Agent-provided numbers are
also untrusted. Until an adapter supplies an objective counter, the metric is
explicitly `unavailable`, never zero. Process sampling therefore closes
Runtime CPU/RSS visibility independently; Provider/sandbox attribution and
cross-restart aggregation remain separate work.

## Service and presentation layers

`sylvander-api` is the pure versioned wire/schema crate. Its former Tokio bus
and in-process implementation have been removed.

`sylvander-server` owns listeners, authentication middleware, request limits,
shutdown, and Runtime construction. Channels translate native transports and
call the Runtime-owned `ChannelHost`; they do not read Session storage or Agent
internals.

TUI and desktop are API clients. They render protocol state and submit typed
commands, but do not depend on Agent, provider adapters, Runtime persistence,
or execution implementations. An embedded single-process distribution still
uses the same API/application boundary through an in-process service adapter.

## Closed extension policy

Until the core product contract stabilizes, the following are built-in and
closed:

- storage backends and schema ownership;
- observability sinks;
- sandbox drivers and execution-target registration;
- authentication and authorization policy engines;
- provider credential sources.

Internal traits exist for separation and testing, not as public plugin APIs.
MCP remains the explicit external tool interoperability boundary and is
reviewed separately.

## Architectural verification

`scripts/verify-architecture.sh` enforces the currently machine-checkable
dependency and source boundaries and is invoked by the release security gate:

- Agent's only first-party dependency is `sylvander-llm-core`;
- API has no Tokio, Agent, Runtime, provider, database, or network dependency;
- Channels and presentation clients have no Agent dependency;
- provider adapters depend only on `sylvander-llm-core` among Sylvander crates;
- only Runtime joins Agent and API;
- concrete SQLite, OCI, SSH, MCP, and network clients do not appear in Agent;
- the deleted Protocol `types` path cannot return, and boundary crates cannot
  introduce function-local Rust imports.

The remaining behavioral gate—every public client completion requires both a
committed Runtime storage outcome and a terminal observability fact—must be
proved by Runtime integration tests rather than inferred from Cargo metadata.
