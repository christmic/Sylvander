# Sylvander Server Agent Platform

Status: normative architecture and production backlog

Last audited: 2026-07-15

Scope: Agent, runtime, public service protocol, execution, workspaces, channels,
run evidence, and the self-improvement loop

## 1. Product contract

Sylvander is a server-owned Agent platform. An Agent is a persistent organic
unit with identity, instructions, memory, capabilities, a home workspace, and a
default model. A session is an interaction with that Agent. It may override
selected runtime settings without mutating the Agent definition.

Clients never embed or own Agent logic. TUI, desktop, DingTalk, Telegram, and
future clients use a versioned service protocol to discover Agents, create or
resume sessions, select allowed models, provide task workspaces, answer
decisions, and observe execution.

The target is zero known open production defects and zero unchecked items in
this document. Completion requires implementation, automated tests,
documentation, migration coverage, and runtime evidence. A type or UI
placeholder is not implementation.

## 2. Non-negotiable invariants

1. **Agent definition is durable.** Identity, default model, prompt policy,
   memory, home workspace, tools, Skills, MCP, and safety policy survive server
   restarts.
2. **Session configuration is an overlay.** Model, reasoning, task workspace,
   execution target, and explicitly permitted prompt overrides belong to one
   session. They never change another session or the Agent default.
3. **The server is authoritative.** Clients request mutations; the server
   validates, persists, applies, and reports the resulting effective state.
4. **One public protocol exists.** Transport adapters do not invent private
   variants of Agent or session semantics.
5. **Execution location is transparent.** Agent logic and tools operate on an
   execution interface, not directly on the server filesystem. Local, SSH,
   container, and sandbox targets share the same contract.
6. **Workspace is composable.** The Agent home and user task workspace may both
   be visible. Capability policy controls operations; a single hard-coded root
   does not define the product model.
7. **Coding is isolated by default.** A mutable Git task receives a worktree
   lease unless the user explicitly selects direct mode. Merge is a separate,
   reviewable operation.
8. **Channels are instances, not singletons.** Multiple bots of the same type
   have distinct identity, credentials, routes, sessions, health, and
   lifecycle.
9. **Evidence is durable and private by design.** Structured run facts support
   recovery, diagnosis, evaluation, and improvement without making raw logs or
   sensitive content the only source of truth.
10. **Self-improvement is gated.** Sylvander may analyze its own evidence and
    prepare changes in an isolated worktree. It may not silently modify or
    merge the running production version.

### 2.1 Pre-release compatibility and memory activation policy

Until a stable release declares otherwise, new implementation targets only
the current schema and current public/internal interfaces. Compatibility
adapters, fallback behavior, automatic data migration, and downgrade paths are
not implicit requirements. They may be added only when the user explicitly
approves their source version, target version, failure policy, test matrix, and
removal plan. An older or unknown durable schema fails closed; the runtime must
not repair it heuristically or substitute an ephemeral backend.

Production long-term memory is a durable store opened by the Runtime
composition root and explicitly injected into every active and historical
Agent revision that uses it. `AgentSpec.memory_stores` is declarative metadata,
not authority for `AgentRunBuilder` to open a database. `InMemoryMemoryStore`
is limited to tests, fixtures, and an explicitly selected ephemeral development
mode; it is never a production fallback. Platform inspection reports only the
Runtime-injected backend as `Active` and keeps unactivated declarations
`Configured` without exposing storage paths.

## 3. Target domain model

### 3.1 AgentDefinition

An immutable, versioned definition contains:

- `agent_id`, display identity, description, and definition revision;
- default `ModelSelection` and provider-independent reasoning settings;
- model-specific `PromptProfile` selection and optional shared prompt layers;
- durable memory profile and retention policy;
- `agent_workspace`, declared workspace mounts, and default execution target;
- built-in tools, Skills, MCP servers, hooks, and capability policy;
- allowed session overrides and channel routing policy;
- secret references, never secret values in inspectable protocol objects.

Updating an Agent creates a new validated revision. Existing sessions retain
their recorded execution composition until explicitly migrated or resumed
under a declared migration policy. The active server safety and access policy
is a live floor rather than historical session state: activating a stricter
policy may immediately revoke access to an older session.

### 3.2 SessionDefinition and EffectiveSessionConfig

A durable session contains:

- `session_id`, `agent_id`, `agent_revision`, owner/tenant identity, lifecycle,
  and timestamps;
- task workspace references and execution target reference;
- optional model, reasoning, prompt-profile, and permission overrides;
- worktree lease, channel bindings, and recovery state;
- a resolved configuration snapshot recorded at each turn boundary.

Resolution is deterministic:

```text
server safety floor
  -> Agent definition defaults
  -> channel policy
  -> persisted session overrides
  -> turn-scoped inputs explicitly allowed by policy
  = immutable EffectiveSessionConfig for one turn
```

An active turn never changes underneath itself. Updates apply to the next turn.

### 3.3 Workspace and execution references

`WorkspaceRef` is logical and serializable:

- stable id and display label;
- execution target id;
- target-relative path or resource URI;
- role: Agent home, task, dependency, artifact, or scratch;
- capabilities and visibility;
- optional Git repository identity.

`ExecutionTarget` selects a configured executor:

- local process;
- SSH host;
- container runtime;
- managed sandbox.

Tools receive an `ExecutionContext` and use executor operations such as stat,
read, write, list, spawn, and Git. They do not branch on SSH/container details.
Paths are checked by the executor against declared mounts and capability
policy. This preserves broad useful access without treating path traversal as
unrestricted authority.

### 3.4 Instruction platform

Instruction assembly is deterministic and inspectable:

1. server safety and protocol instructions;
2. Agent prompt profile selected for the effective model/provider;
3. Agent-home `AGENTS.md` hierarchy;
4. task-workspace `AGENTS.md` hierarchy;
5. activated Skill instructions;
6. session/turn prompt input if the Agent policy permits it.

Every layer has provenance, precedence, size limits, and a content digest.
Secrets are excluded. `CLAUDE.md` compatibility may be supported as an explicit
alias, but `AGENTS.md` is the canonical portable contract.

Skills are discoverable packages with activation rules and trust metadata. MCP
is a supervised runtime with transport, tool/resource discovery, health,
timeouts, restart policy, authentication references, and namespaced tools.

### 3.5 Worktree lease

A Git coding session creates or attaches a durable `WorktreeLease`:

- source repository and base revision;
- generated collision-free branch and worktree path;
- owning Agent/session/turn;
- lease state, dirty status, validation evidence, and expiration;
- review, merge, abandon, and cleanup transitions.

The executor performs Git operations. The Agent sees ordinary tool operations
inside the task mount. Concurrent Agents cannot receive the same mutable lease.

### 3.6 Channel instance and route

`ChannelInstance` contains a stable id, channel kind, secret references,
endpoint configuration, default Agent route, workspace/executor policy,
allowlists, health, and lifecycle settings.

External identity keys are composite:

```text
(channel_instance_id, external_account_id, external_conversation_id)
```

Inbound updates are deduplicated. Outbound subscriptions are scoped to the
instance's bound sessions. One failed instance is supervised independently and
cannot stop other bots or the Agent runtime.

### 3.7 Run ledger and improvement loop

The run ledger is a structured fact store, not a log archive. Minimum records:

- run, turn, step, model request, tool call, decision, artifact, validation,
  feedback, and terminal outcome;
- Agent/session/revision/model/executor/workspace correlation;
- monotonic timing, token/cost data, retries, timeouts, cancellation, and
  recovery information;
- redacted inputs/outputs or content digests according to data classification;
- links to generated diffs, test reports, and worktree revisions.

Required controls:

- schema versioning, idempotent append, crash-safe terminal states;
- configurable retention, sampling, encryption, export, and deletion;
- secret and personal-data redaction before persistence;
- tenant/user isolation and purpose-limited access;
- an explicit feedback API for correctness, usefulness, and preference signals.

The improvement loop is:

```text
evidence selection -> failure/quality analysis -> improvement proposal
-> reproducible evaluation set -> isolated worktree change
-> baseline/candidate comparison -> human review -> merge or reject
```

No production change is accepted solely because the same model that authored
it judged it successful. Deterministic tests and, where appropriate, held-out
evaluation data are required.

## 4. Current implementation audit

Legend: `implemented`, `partial`, `missing`, `defect`.

| ID | Area | Status | Evidence and gap |
|---|---|---:|---|
| A01 | Agent specification | partial | The production runtime loads validated configured Agent definitions, including access, model, prompt, workspace, tools, and MCP declarations. The definition surface still needs the P1/P2 prompt, memory, Skill, and MCP runtimes. |
| A02 | Agent registry | implemented | SQLite persists immutable, integrity-checked Agent revisions and an optimistic active head. Redacted public administration supports inspect, update, activate, and rollback; the runtime precomposes candidates, hot-loads active revisions, preserves historical execution workers, restores them after restart, and audits mutations. |
| A03 | Runtime composition | implemented | `sylvander-server` delegates boot, durable storage, Agent/channel startup, readiness, failure reporting, and bounded drain to `sylvander-runtime`. |
| A04 | Session model override | implemented | Model and reasoning overrides are durable session configuration. Current wire uses qualified `(provider_id, model_id)` identity; legacy bare ids resolve only when unique. TUI, Unix, and WebSocket require a session ID and use optimistic updates; ambiguous, unavailable, and unscoped requests fail before mutation. |
| A05 | Session permission override | implemented | Permission profiles are durable session overrides and do not mutate `AgentRun` global state; real-runtime tests cover two-session isolation. |
| A06 | Model providers | partial | Production Agent runs use the provider-neutral request/stream contract, immutable Provider/Model registry snapshots, request-scoped Credential resolution, and provider-backed compaction. Public UI v3 administration provides strict write drafts and typed errors for Provider/Model/Credential lifecycle operations, SQL CAS, full-row canonical/digest integrity checks, Provider adapter preflight, and durable mutation intent plus terminal audit. Registry-declared canonical capabilities, lifecycle, and pricing are published through the exact provider-qualified runtime catalog; adapter and request preflight fail closed before credential resolution or dispatch. Optional provider-native catalog synchronization and additional adapter implementations remain. |
| A07 | Model-specific prompts | implemented | One resolver composes the non-overridable safety floor, exact provider/model profile, Agent prompt, and allowed session input with strict limits and ordered digests. The immutable manifest survives restart and is revalidated before turn persistence, history mutation, tools, compaction, or provider dispatch. Public responses expose digests but keep raw session prompt input write-only. |
| A08 | Agent workspace | partial | Configured Agent home and a user task workspace resolve into effective session state. Multiple role-bearing mounts and backend-neutral composition remain in P2.1. |
| A09 | File tools | partial | Read/Write/Edit enforce capabilities and a canonical local root, but call `std::fs` directly and cannot address remote/container/sandbox resources. |
| A10 | Command/Git tools | missing | The Agent has no production spawn/shell/Git tool surface. `Cap::Spawn` and `Cap::Git` are declarations without executor-backed tools. |
| A11 | Worktree isolation | missing | No worktree lease, branch lifecycle, merge gate, ownership, or cleanup service exists. |
| A12 | AGENTS.md | missing | Repository guides exist for developers, but the running Agent does not discover or assemble workspace instructions. |
| A13 | Skills | missing | Protocol/UI placeholders can display Skills, but the Agent has no Skill discovery, trust, activation, or instruction loading runtime. |
| A14 | MCP | defect | MCP configuration types and UI inspection exist, but no MCP process/client, discovery, execution, health, or resource implementation exists. The UI correctly reports configuration only. |
| A15 | Agent memory | partial | Production boot opens one durable SQLite relationship-memory store and injects the same `Arc` into initial, active, historical, revalidated, activated, and rolled-back Agent revisions. Typed runtime ownership isolates `(user, Agent)`; only a Runtime-issued, run-bound authenticated session can obtain memory authority, while raw bus joins remain untrusted. Revision, immutable provenance, bounded trace digest, policy revision, and effective expiry survive restart. The exact latest schema fails closed and never falls back to InMemory. CAS update/delete, atomic non-dangling supersession, finite default/max TTL, and bounded physical purge are transaction-coupled to content-safe per-record and run audit. A persistent monotonic watermark prevents rollback from reviving expired data; dangerous forward jumps enter a durable quarantine that blocks purge until maintenance explicitly confirms the clock. Runtime owns startup catch-up, periodic retention, authenticated scheduled backup rotation, and bounded shutdown. Every production mutation advances a keyed, external epoch/root anchor; startup detects row, audit, deletion, and database rollback tampering. Offline restore accepts only a signed backup at the currently anchored epoch. Host-administrator rollback still requires a remote monotonic CAS anchor backend; the file backend proves the restricted database-writer boundary only. |
| A16 | Public service protocol | complete | UI v3 messages are owned by `sylvander-protocol`, shared by Unix/WebSocket/TUI, generated as JSON Schema, and compatibility-tested across v1/v2/v3 negotiation and legacy message defaults. External Channels receive subscribe-only bus access; authenticated chat and interactive controls enter through Runtime-owned UI operations. A new chat subscribes before exactly one publish, and any creation, metadata, Engine, subscription, or dispatch failure compensates durable session, Engine attachment, and AgentRun authority without deleting an existing session. |
| A17 | Session persistence | partial | SQLite persists sessions, messages, usage, archive/fork/compaction, sparse overrides, immutable Agent/Provider/Model revision pins, effective prompt/permissions/workspaces/executor, and channel ownership metadata. Restart deterministically closes legacy pins and execution revalidates them against the snapshot. General mount sets and worktree leases remain P2/P3. |
| A18 | Identity and authorization | partial | Protocol-owned authenticated transport principals, default-deny Agent access, session ownership, per-operation policy, boundary limits, typed denials, and content-free denial audit are enforced. A latest-only stable `UserId`/`PrincipalBinding` store now provides HMAC-keyed channel-instance isolation, explicit single-use link challenges, and monotonic unlink/relink CAS. Runtime ownership and authenticated Channel protocol wiring remain open; transport principal strings are not yet replaced by stable user identity at ingress. |
| A19 | DingTalk instances | partial | Configuration supports multiple credential-isolated bots; sender/conversation mappings, ownership, authorization, and outbound webhooks are instance-scoped. Interactive decisions, retry policy, and operational health remain in P4.2. |
| A20 | Telegram instances | partial | The server constructs configured bots using the shared durable store, required webhook authentication, instance-scoped principals/chat mappings, authorization, and Unicode-safe chunking. Interactive decisions, retries, and operational health remain in P4.3. |
| A21 | Other channels | partial | The production server constructs configured Unix, HTTP, WebSocket, DingTalk, Telegram, and WeChat instances. Uniform supervision, interactive operations, retries, and health remain in P4. |
| A22 | Channel supervision | partial | DingTalk reconnects internally, but runtime-wide instance health, restart backoff, readiness, drain, and failure isolation are not modeled. Some channel startup paths unwrap. |
| A23 | Run evidence | implemented | The durable run ledger correlates runs, turns, steps, outcomes, usage, tool activity, recovery, retention, queries, feedback, and content-free administration/authorization audit. Raw content is governed separately from structured evidence. |
| A24 | Feedback | complete | Typed positive/negative feedback is accepted through the public UI service and persisted only when it references a real evidence run and, optionally, a turn belonging to that run. |
| A25 | Self-improvement | missing | There is no evidence selection, evaluation corpus, proposal, experiment, comparison, or human merge gate. |
| A26 | Data governance | missing | Run-data classification, redaction, encryption, retention, deletion, export, and cross-tenant isolation policy are not implemented. |
| A27 | Secrets | partial | Typed environment/file references, bounded zeroizing values, request-scoped Provider resolution, immutable generations, live rotation, activation preflight, and redacted administration are implemented. External secret backends, lease renewal, and uniform channel-credential rotation remain. |
| A28 | Database migrations | partial | Registry components and relationship memory have explicit component ledgers. Relationship memory accepts only its exact latest schema and rejects unmanaged, older, future, or damaged layouts without repair or fallback. This is deliberate latest-only validation, not migration support. Session/evidence schema convergence, backup/restore drills, and any explicitly approved upgrade or downgrade migration remain. |
| A29 | Shutdown and recovery | partial | Runtime boot restores durable sessions and interrupted evidence, and shutdown uses bounded cooperative channel/Agent drain. Full crash orchestration, executor leases, and per-instance recovery policy remain incomplete. |
| A30 | Observability and operations | partial | Structured tracing and health endpoints exist. Metrics, readiness dependencies, per-instance health, queue/backpressure visibility, audit export, and operational diagnostics are incomplete. |
| A31 | Concurrency isolation | implemented | Agent turns use per-session locks and active-turn cancellation; real-runtime PTY tests cover multi-client session isolation. This must remain a regression gate. |
| A32 | Approval authority | partial | The Agent owns approval decisions and durable fingerprints can be configured. Permission selection remains global and persistent approvals need identity/policy scoping. |
| A33 | Mutation recovery | partial | The workspace journal supports local Write/Edit rollback. It is not executor-neutral and does not replace worktree isolation for coding. |
| A34 | Test depth | partial | Agent, protocol, TUI, PTY, and real-runtime coverage is substantial. There is no executor contract suite, multi-instance channel suite, crash/restart ledger suite, config migration suite, or self-improvement evaluation suite. |

## 5. Prioritized executable backlog

Items are completed in order unless a later test-only item can run safely in
parallel. An item becomes `done` only when its acceptance evidence is linked.

### P0 — Correctness, safety, and durable contracts

- [x] **P0.1 Configuration schema and loader:** versioned server config,
  Agent definitions, model providers, execution targets, channel instances,
  secret references, validation, redacted inspection, and environment migration.
  Evidence: `sylvander-runtime/src/config`, `config/sylvander.example.toml`, and
  `docs/server-configuration.md`; runtime tests cover validation, secrets,
  migration, the maintained example, composition, and durable restart.
- [x] **P0.2 Production composition root:** make `sylvander-runtime` the only
  boot path; use the configured durable store; supervise Agents and channels;
  graceful drain and explicit startup failures.
  Evidence: runtime-owned task handles, readiness handshakes, transactional
  channel startup, unexpected Agent/channel exit reporting, bounded cooperative
  drain, and a real Unix/HTTP server startup-health-shutdown smoke test.
- [x] **P0.3 Session effective configuration:** persist Agent revision, model,
  reasoning, permissions, prompt profile, workspaces, executor, and override
  provenance; snapshot atomically per turn; migrate existing sessions.
  Evidence: protocol-owned sparse/effective/provenance types; dedicated SQLite
  columns and immutable turn snapshots; runtime resolution across Agent,
  channel, session, and legacy workspace layers; optimistic revision updates;
  boot migration; and an end-to-end model request proving that the persisted
  model/prompt/permission selection is used before the provider or tools run.
  The configured execution-target identity is durable here; backend-neutral
  filesystem/process execution remains deliberately tracked by P3.1/P3.2.
- [x] **P0.4 Public protocol v3:** move service messages into
  `sylvander-protocol`; add Agent discovery, session create/update/effective
  state, feedback, and optimistic concurrency; generate and compatibility-test
  the schema. Evidence: one shared `UiClientMessage`/`UiServerMessage` contract
  across Unix, WebSocket, and TUI; runtime-owned `UiService`; durable configured
  session creation and optimistic updates; evidence-linked feedback; Schemars
  v3 generation; and v1/v2/v3 negotiation plus schema compatibility tests.
- [x] **P0.5 Boundary authorization:** authenticated principals, Agent/session
  ownership, channel-instance identity, policy checks, safe defaults, rate and
  payload limits, and auditable denials. Evidence: protocol-owned boundary
  types; Unix peer, HTTP/WebSocket bearer, and signed platform authentication;
  runtime-owned authorization before dispatch; default-private Agent policy;
  instance-scoped platform identities, replay suppression, and outbound
  routing; typed client denials; authentication-failure rate limiting; durable
  content-free audit; transport payload limits; session-scoped model and
  permission controls; cross-principal/instance integration tests; and
  [`boundary-authorization.md`](boundary-authorization.md).
- [x] **P0.6 Run ledger foundation:** durable run/turn/step/outcome records,
  correlation, crash recovery, redaction, retention, and query APIs before new
  execution backends multiply evidence sources.
  Evidence: `sylvander-runtime/src/evidence.rs`, its bus recorder, persistence,
  recovery/retention/query tests, and `docs/runtime-evidence.md`.

### P1 — Persistent Agent organism

- [x] **P1.1 Agent registry and revisions:** load, validate, persist, inspect,
  update, activate, roll back, and bind sessions to revisions.
  Evidence: immutable SQLite revisions and active heads with digest validation;
  redacted protocol administration over Unix/WebSocket; admin/system
  authorization; optimistic conflicts; atomic staging of each immutable Agent
  definition and its exact V3 Provider/Model closure in one immediate
  transaction; full composition revalidation before active-head mutation; hot
  activation, rollback, historical session workers, restart restoration;
  provider-request tests for revision-specific model/prompt selection; and
  durable content-free success/failure audit.
- [x] **P1.2 Provider and model registry:** provider-neutral client factory,
  Registry capability publication and request validation, per-Agent default,
  per-session override, lifecycle, pricing, and credential references.
  - [x] Provider-neutral model, conversation, tool/media, stream, error, usage,
    and capability contracts are independent from public UI protocol types.
  - [x] Anthropic implements the neutral adapter with no internal retry or
    fallback, redacted errors, and exactly one terminal completion.
  - [x] Public and durable session selection is provider-qualified; legacy ids
    are accepted only when the visible catalog has one exact match.
  - [x] Migrate production `AgentLoop` through a compatibility-preserving dual
    backend; legacy history, events, tools, and builders remain valid, while
    provider streams are checked for one terminal completion and exact model
    identity. Manual and automatic compaction use the same pinned provider
    backend and return typed, redacted failures.
  - [x] Extend the existing `sessions.db` Agent registry SSOT with component
    migrations and immutable Provider/Model/Credential revision tables. Do not
    create a second registry database.
    Evidence: the component migration ledger plus integrity-checked registry
    domain loaders in `sylvander-runtime/src/agent_registry.rs` and
    `sylvander-runtime/src/registry_domain.rs`.
  - [x] Add true SQL compare-and-swap across multiple registry connections,
    integrity validation, restart migration, lifecycle, and pricing metadata.
    Provider/Model/Credential heads use optimistic SQL updates and immutable
    digest-checked definitions in the existing `sessions.db` SSOT.
  - [x] Route active Provider/Model revisions dynamically while sessions pin
    definition revisions; credential bindings rotate live by generation and
    never persist resolved secret values.
    Evidence: immutable Agent registry snapshots, exact production
    `RuntimeRevisionProvider` composition, persisted Provider/Model session
    pins, deterministic legacy closure, execution-boundary revalidation, and
    request-scoped credential rotation tests in `sylvander-runtime`.
  - [x] Expose redacted Provider/Model/Credential revision inspection through
    the public protocol with transport authorization, service authorization,
    immutable exact-version reads, bounded database pagination, and durable
    content-free audit.
  - [x] Expose Credential create/stage/activate/rollback with strict immutable
    generations, optimistic head concurrency, exact-generation availability
    preflight, typed redacted failures, durable pre-mutation intent and terminal
    audit, UI protocol v3 negotiation, and Unix/WebSocket round trips.
  - [x] Expose Provider/Model create/stage/activate/rollback through strict UI
    protocol v3 drafts with typed redacted failures, SQL compare-and-swap,
    full-row identity/revision/canonical-JSON/digest verification, Provider
    adapter preflight, and durable pre-mutation intent plus terminal audit.
    Component head mutations are future-only: they do not rewrite existing
    Agent snapshots or sessions. Adopting a new component requires creating and
    promoting a fully precomposed Agent revision through the existing Agent
    administration path.
  - [x] Canonicalize Registry Model capabilities and publish protocol-owned
    names through the exact provider-qualified runtime catalog. Malformed or
    adapter-unsupported declarations fail during preflight, and request/model
    capability mismatches fail before credentials or adapter dispatch without
    fallback.
  - [x] Enable validated cross-provider session overrides and prove same model
    ids across providers, historical sessions, restart, rotation, and failure
    isolation with deterministic local providers. Evidence includes public
    RegistryAdmin and AgentAdmin adoption, DiscoverAgents metadata, ambiguous
    legacy selection rejection before mutation, exact session revision pins,
    restart restoration, live Credential rotation, and one-provider failure
    without fallback or contamination of a healthy Provider.
- [x] **P1.3 Prompt resolver:** shared safety layers, exact qualified
  model/provider profiles, Agent prompt, allowed write-only session input,
  ordered provenance/digests, byte limits, deterministic restart, and
  execution-boundary tamper rejection. Evidence: `sylvander-agent/src/prompt.rs`,
  `sylvander-agent/src/run.rs`, protocol schema/redaction tests, real Unix and
  WebSocket response tests, provider-wire composition tests, and runtime
  restart acceptance in `registry_agent_composition_tests.rs`.
- [ ] **P1.4 Durable memory:** durable Agent-owned memory lifecycle and
  governance. This item remains open; the completed storage foundation is not
  the full lifecycle.
  - [x] Derive relationship ownership and provenance from trusted runtime
    context, not model input; isolate records by `(user_id, agent_id)` and keep
    missing and foreign records indistinguishable.
  - [x] Persist relationship memory in the production SQLite database with
    bounded retrieval/write/delete inputs, revision, absolute expiry,
    supersession linkage, immutable provenance, and content-safe append-only
    audit storage.
  - [x] Open one durable store in `Runtime::boot_config` and inject the same
    instance into all revision compositions. Agent declarations never open a
    backend, and production never falls back to `InMemoryMemoryStore`.
  - [x] Enforce the exact latest relationship-memory schema. Unmanaged, older,
    future, missing-trigger, or malformed schemas fail startup without repair;
    restart tests preserve owner isolation, revision, provenance, and expiry.
  - [x] Add public/internal compare-and-swap update with immutable origin,
    monotonic revision, atomic audit, conflict tests, and no partial mutation.
  - [x] Add an atomic supersede transition that links old and replacement
    records, hides the old record from ordinary recall, and records both sides
    without a visibility gap.
  - [x] Implement configured retention and physical purge/deletion governance,
    including bounded batches, authorization, crash recovery, and exact audit
    assertions for update, supersede, expiry, purge, and delete transitions.
  - [x] Add backup/restore verification and only those schema/data migrations
    that receive explicit approval under section 2.1.
  - [x] Activate a changed retention-policy revision only after full Runtime
    readiness; a failed rollout must leave the previous revision reusable.
  - [ ] Bound audit and retention-ledger growth only after a verified external
    checkpoint preserves the evidence required for recovery and inspection.
  - [ ] Add a remote monotonic CAS anchor backend for deployments whose threat
    model includes a host administrator replaying database, file anchor, and
    key together.

  Production integrity boundary:

  - `server.memory_maintenance.integrity.key` and the tagged `backend` are
    mandatory. `key` is common to every backend and is resolved from an
    environment/file secret reference; it is never stored in SQLite, an
    anchor, a backup manifest, logs, Debug output, or errors.
  - `backend.kind = "file"` requires an absolute `anchor_path`. Its parent must
    already exist, and Runtime rejects paths that resolve inside
    `server.data_dir`. The anchor directory must be mounted or permissioned so
    the identity that can write `memory.db` cannot write, delete, or roll back
    the anchor.
  - The file backend uses authenticated epoch/root records and atomic initial
    creation. It detects a restricted database writer changing/deleting rows,
    deleting audit history, replaying an older database, forging a manifest,
    or restoring an older backup epoch. It does **not** defeat a host
    administrator who can replay both the database and an older valid anchor.
  - `backend.kind = "http"` delegates the monotonic ledger to a separately
    administered HTTPS CAS service. The endpoint cannot contain credentials,
    a query, or a fragment. Bearer credentials are mandatory secret
    references; private CA and client-identity references are optional.
    Timeouts are bounded to 100–60000 ms and read retries to at most 10. CAS
    conflicts and ambiguous writes fail closed rather than being converted
    into blind retries. This backend is the required deployment shape when the
    threat model includes whole-host historical replay.
  - Mutations use a two-phase anchor transition: a signed
    `Pending{from_epoch/root,to_epoch/root}` is fsynced before SQLite commit,
    then finalized after commit. Restart accepts only the authenticated
    `from` root (transaction rolled back) or `to` root (transaction committed)
    and deterministically repairs the anchor; every third state fails closed.
    Read-only recall never scans the full database or rewrites the anchor.
  - Schema v6 stores an epoch-bound HMAC for every model-visible memory row and
    an externally anchored, replaceable retention-policy stage. Policy
    activation is CAS-bound to the active base revision and happens only after
    Runtime readiness succeeds; failed rollout stages never reserve a revision.
    Insert, update, supersede, delete, and maintenance transactions re-seal
    rows before prepare/commit. `get` and `search` verify only returned rows
    against the committed anchor epoch, so an online database writer cannot
    feed forged or replayed row content to the model between checkpoints.

  Generated memory IDs, internal record keys, audit event IDs, retention run
  IDs, and retention batch IDs are allocated inside their mutation transaction
  with bounded collision retries. Exhaustion returns one content-safe storage
  failure and commits no partial mutation.

  Current evidence: `acd5ab661` (runtime-derived ownership), `73316754f` and
  `e0ebfaae5` (SQLite persistence and contract tests), `1d11d8fc9` (one store
  across revisions), `0977357ec` (truthful activation reporting), `75d280b15`
  and `c5c4efc73` (latest schema, append/delete audit, and fail-closed schema
  tests), `ccc8b75ab` (production boot, owner isolation, and restart field
  fidelity), and `6b245d052` plus `7251f8336` (CAS update, atomic supersession,
  audit rollback, and inactive-record isolation), `fd95f67c6` (schema-v5
  authenticated external anchor and signed backup epochs), and `7758a1b`
  (transactional generated-identifier collision handling), and `94bfeb4`
  through `6c6a207f0` (readiness-gated policy activation and cross-store anchor
  writer serialization). G0 must not be marked complete until every remaining P1.4 gate
  above has implementation and acceptance evidence.
- [ ] **P1.5 Stable user identity and account binding:** make Runtime own the
  latest-only stable user/principal store and its external HMAC key. Channel
  ingress derives typed external principals only after platform
  authentication; public clients and models cannot self-assert one or access
  the store. Expose explicit, expiring, single-use begin/confirm and
  owner-authorized CAS unlink operations through the versioned UI service.
  Storage-domain evidence: `367214999` through `e86abd1a2`; Runtime/Channel
  composition and end-to-end transport tests remain.
- [ ] **P1.6 Optional Provider catalog synchronization:** let adapters that
  expose a remote model catalog enumerate it, reconcile discovered metadata
  against the Registry SSOT, report drift and health, and never silently
  rewrite an active Agent snapshot. Providers without a reliable enumeration
  contract continue to use validated operator-managed Registry metadata.

### P2 — Workspace and extension platform

- [ ] **P2.1 Workspace composition:** Agent home plus task/dependency/artifact
  mounts, logical references, capability policy, collision rules, and effective
  workspace inspection.
- [ ] **P2.2 AGENTS.md resolver:** hierarchical discovery across Agent/task
  workspaces, precedence, aliases, size limits, provenance, cache invalidation,
  and prompt integration.
- [ ] **P2.3 Skills runtime:** package format, discovery, trust, activation,
  instruction/resource loading, validation, health, and protocol inspection.
- [ ] **P2.4 MCP runtime:** supervised transports, initialization, tool/resource
  discovery, namespacing, auth references, timeouts, cancellation, restart,
  health, and safe tool adaptation.

### P3 — Location-transparent execution and coding isolation

- [ ] **P3.1 Executor contract:** local reference implementation plus a shared
  conformance suite for filesystem, process, environment, cancellation,
  streaming, limits, and Git operations.
- [ ] **P3.2 Executor-backed tools:** migrate Read/Write/Edit and add bounded
  List/Search/Command/Git operations without exposing backend location to the
  Agent loop.
- [ ] **P3.3 SSH executor:** host-key policy, connection pooling, credential
  references, cancellation, upload/download semantics, and conformance tests.
- [ ] **P3.4 Container and sandbox executors:** lifecycle, mounts, resource and
  network policy, cleanup, and the same conformance tests.
- [ ] **P3.5 Worktree manager:** default lease creation for Git coding,
  collision-free ownership, validation evidence, review/merge/abandon flow,
  recovery, and garbage collection.

### P4 — Multi-instance channels

- [ ] **P4.1 Channel registry and supervisor:** typed instance configuration,
  composite external identity, scoped bus subscriptions, health/restart/drain,
  deduplication, and route policy.
- [ ] **P4.2 DingTalk production adapter:** multiple bots, instance-isolated
  sessions/credentials, interactive decisions, retries, limits, and tests.
- [ ] **P4.3 Telegram production adapter:** remove the private store, support
  multiple webhook instances/routes, verify webhook authenticity, interactive
  decisions, Unicode-safe chunking, retries, limits, and tests.
- [ ] **P4.4 Remaining adapters:** apply the same instance/auth/supervision
  contract to Unix, HTTP, WebSocket, and WeChat without weakening the public
  protocol.

### P5 — Evidence-driven self-improvement

- [ ] **P5.1 Feedback and outcome APIs:** user ratings/corrections, task result,
  artifact and validation links, privacy class, and immutable attribution.
- [ ] **P5.2 Analysis pipeline:** reproducible cohorts, failure taxonomy,
  quality/cost/latency/tool/approval/recovery metrics, and bias/leakage checks.
- [ ] **P5.3 Evaluation registry:** versioned datasets, deterministic fixtures,
  held-out cases, scoring adapters, baselines, and regression thresholds.
- [ ] **P5.4 Improvement proposal:** evidence-linked hypotheses with expected
  benefit, risk, affected components, rollback, and required evaluations.
- [ ] **P5.5 Self-change experiment:** isolated worktree implementation,
  baseline/candidate execution, signed evidence bundle, human approval, merge,
  deployment observation, and automatic rollback criteria.

### P6 — Production closure

- [ ] **P6.1 Migration and recovery drills:** schema/config migrations,
  interrupted upgrades, backup/restore, executor/channel restart, and orphan
  worktree/run recovery.
- [ ] **P6.2 Operational controls:** health/readiness, metrics, tracing export,
  queue/backpressure limits, quotas, diagnostics, alerts, and runbooks.
- [ ] **P6.3 Security verification:** threat model, secret scanning, dependency
  audit, protocol fuzz/property tests, path/command injection tests, tenant
  isolation, and data deletion verification.
- [ ] **P6.4 Performance verification:** concurrency, long sessions, large
  workspaces, slow remote execution, channel bursts, resource ceilings, and
  latency budgets.
- [ ] **P6.5 Final closure:** full clean-room deployment, real-client journeys,
  zero known critical/high defects, no unchecked backlog item, and an explicit
  residual-risk record for anything that cannot be proven away.

## 6. Verification policy

Every implementation batch must include:

1. unit tests for domain invariants and validation failures;
2. integration tests across the public boundary it changes;
3. persistence/restart tests for durable state;
4. negative security and cross-session/instance isolation tests;
5. formatting, clippy with warnings denied, workspace tests, and release build;
6. documentation and migration notes in the same batch;
7. a reversible commit that does not mix unrelated subsystems.

Real credentials are never required for the default suite. Provider, SSH,
container, MCP, DingTalk, and Telegram contracts use deterministic local fakes;
credentialed smoke tests are opt-in and redact all captured data.
