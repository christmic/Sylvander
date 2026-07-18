# Sylvander Server Agent Platform

Status: normative architecture and current implementation audit

Last audited: 2026-07-18

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
the active
[`production-expansion-checklist.md`](production-expansion-checklist.md).
Completion requires implementation, automated tests, documentation, and
runtime evidence. A type or UI placeholder is not implementation.

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
is limited to tests and fixtures; it is not a server mode or production
fallback. Platform inspection reports only the Runtime-injected backend as
`Active` and keeps unactivated declarations `Configured` without exposing
storage paths.

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

Skills are discoverable packages with activation rules and trust metadata.
The current package contract is documented in
[`sylvander-agent/docs/skills.md`](../sylvander-agent/docs/skills.md). MCP is a
supervised runtime with transport, tool/resource discovery, health, timeouts,
restart policy, authentication references, and namespaced tools.

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
| A01 | Agent specification | implemented | Immutable, integrity-checked Agent revisions cover identity, access, qualified model defaults and explicit non-empty allowlists, prompt profiles, relationship-memory declarations, Agent home and role-bearing mounts, built-in/MCP tools, hooks, UI commands, behavior, and presentation metadata. The default must be present in the qualified allowlist; Runtime never expands an empty list from a Provider catalog. Runtime precomposition validates the complete active revision and binds its prompt, durable memory, workspace instruction/Skill discovery, MCP, and extension services before activation. |
| A02 | Agent registry | implemented | SQLite persists immutable, integrity-checked Agent revisions and an optimistic active head. Redacted public administration supports inspect, update, activate, and rollback; the runtime precomposes candidates, hot-loads active revisions, preserves historical execution workers, restores them after restart, and audits mutations. |
| A03 | Runtime composition | implemented | `sylvander-server` delegates boot, durable storage, Agent/channel startup, readiness, failure reporting, and bounded drain to `sylvander-runtime`. |
| A04 | Session model override | implemented | Model and reasoning overrides are durable session configuration. Wire and storage use qualified `(provider_id, model_id)` identity. TUI, Unix, and WebSocket require a session ID plus expected configuration revision; bare, unavailable, stale, and unscoped requests fail before mutation. |
| A05 | Session permission override | implemented | Permission profiles are durable session overrides and do not mutate `AgentRun` global state; real-runtime tests cover two-session isolation. |
| A06 | Model providers | implemented for current adapters | Production Agent runs use the provider-neutral request/stream contract, immutable Provider/Model registry snapshots, request-scoped Credential resolution, and provider-backed compaction. Public UI v5 administration provides strict write drafts and typed errors for Provider/Model/Credential lifecycle operations, SQL CAS, full-row canonical/digest integrity checks, Provider adapter preflight, and durable mutation intent plus terminal audit. Registry-declared canonical capabilities, lifecycle, and pricing are published through the exact provider-qualified runtime catalog; adapter and request preflight fail closed before credential resolution or dispatch. The neutral provider contract optionally enumerates a reliable remote catalog. Runtime reconciliation reports synchronized, drifted, unavailable, or operator-managed state and never mutates Registry metadata or active Agent snapshots. Current Anthropic-compatible adapters intentionally remain operator-managed because their deployment endpoints do not guarantee one authoritative catalog. |
| A07 | Model-specific prompts | implemented | One resolver composes the non-overridable safety floor, exact provider/model profile, Agent prompt, and allowed session input with strict limits and ordered digests. The immutable manifest survives restart and is revalidated before turn persistence, history mutation, tools, compaction, or provider dispatch. Public responses expose digests but keep raw session prompt input write-only. |
| A08 | Agent workspace | implemented | Effective session state carries Agent home, task, dependency, and artifact mounts as canonical logical references with independent read/write/command/Git policy. The location-neutral mount router applies collision, overlap, capability, and read-only validation across local, SSH, container, and managed-sandbox targets without exposing backend paths to the Agent. |
| A09 | File tools | implemented for local, SSH, container, and managed sandbox execution | Read/Write/Edit/List/Search use one location-neutral executor and logical mount router with workspace-relative paths, structured bounds, read-only enforcement, and explicit unavailable-target failure. |
| A10 | Command/Git tools | implemented for local, SSH, container, and managed sandbox execution | Command and structured read-only Git share the executor boundary. Local, OpenSSH, and restricted OCI execution support bounded concurrent stdout/stderr capture, Unicode-safe streaming, deadlines and cancellation, plus isolated worktree review/accept/discard. |
| A11 | Worktree isolation | implemented for local, host-backed, and SSH execution | Writable Git sessions receive durable isolated worktrees, diff review, accept merge, discard, and restart recovery. Runtime boot validates every active lease against durable session state and reconciles orphaned or missing leases. SSH leases keep local intent manifests while executing Git operations in the configured remote worktree root. |
| A12 | AGENTS.md | implemented | Every turn discovers bounded `AGENTS.md`, `AGENT.md`, and `agent.md` hierarchies through the selected workspace executor for Agent-home and task mounts. Deterministic alias, root-to-focus, and Agent-before-task precedence is recorded in prompt provenance; per-turn reload supplies cache invalidation for local, SSH, container, and managed-sandbox workspaces. |
| A13 | Skills | implemented | Agent-home and task-workspace packages are discovered through the selected executor with deterministic precedence. Strict optional manifests provide versioned metadata, activation, and exact resources; invalid or incomplete packages fail atomically. Redacted active/configured/degraded health, trust, source, capabilities, and per-turn reload truth are protocol-visible. |
| A14 | MCP | implemented for the local stdio scope | Production composition resolves secret references, supervises configured MCP stdio servers, negotiates the exact protocol, discovers collision-safe namespaced tools and resources, enforces request deadlines, emits protocol cancellation on timeout or interrupted futures, probes health, reconnects without replaying uncertain calls, atomically refreshes catalogs, bounds visible output while routing complete results into the encrypted tenant/user-scoped evidence artifact sink, exposes redacted health/capabilities, and drains owned processes. Optional MCP prompts, subscriptions, and non-stdio transports are not advertised. |
| A14H | Hooks | implemented | The latest-only Agent schema requires one of four executable phases: `before_tool`, `after_tool`, `before_turn`, or `after_turn`. Every phase runs through the selected location-neutral workspace executor with a bounded timeout and control-safe, bounded visible deltas. Blocking before hooks stop the pending operation; blocking after hooks reject publication of the completed tool result or turn; advisory failures remain visible or traced without replacing the underlying result. Commands are redacted from inspection. Hook changes are part of the immutable capability revision and become live only after the new Agent revision is re-composed, validated, and compare-and-swap activated; frozen sessions retain their prior revision while newly bound sessions receive the activated hooks. Inspection reports the exact phase as a reloadable capability. Session phases are deliberately absent until a context-complete executor boundary exists, so the API cannot accept inert hook configuration. |
| A14X | Extensions | implemented | Agent definitions and the public administration contract contribute ordinary tools, typed prompt commands, and bounded declarative tool-presentation metadata. The TUI interprets presentation kind/label/target fields using trusted local renderers, sanitizes all values, retains a visible fallback, and exposes contribution counts/capabilities through `/extensions`; extensions receive neither UI callbacks nor a path around normal tool execution and approval. |
| A15 | Agent memory | implemented | Production boot opens one durable SQLite relationship-memory store and injects the same `Arc` into initial, active, historical, revalidated, activated, and rolled-back Agent revisions. Typed runtime ownership isolates `(user, Agent)`; only a Runtime-issued, run-bound authenticated session can obtain memory authority, while raw bus joins remain untrusted. Revision, immutable provenance, bounded trace digest, policy revision, and effective expiry survive restart. The exact latest schema fails closed and never falls back to InMemory. CAS update/delete, atomic non-dangling supersession, finite default/max TTL, and bounded physical purge are transaction-coupled to content-safe per-record and run audit. A persistent monotonic watermark prevents rollback from reviving expired data; dangerous forward jumps enter durable quarantine until maintenance confirms the clock. Runtime owns startup catch-up, periodic retention, authenticated scheduled backup rotation, checkpoint-authorized bounded evidence compaction, and bounded shutdown. Compacted audit and retention rows form cumulative anchored summary roots while the newest live boundary rows remain. Every production mutation advances a keyed epoch/root anchor: the local file backend closes the restricted database-writer boundary, while the separately administered HTTP strong-CAS backend rejects whole-host historical replay. Offline restore accepts only a signed backup at the currently anchored epoch. Guardian curation uses separate durable outbox, candidate, policy-decision, mutation-delivery, and canonical-memory stores with idempotent restart recovery; semantic classification cannot authorize a mutation. The owner-free `memory_confirmation_v1` UI flow binds a pending candidate to its authenticated owner, originating session, and exact revision before an explicit confirm/reject transition. The live `do_not_learn` preference denies direct relationship append, Worker memory candidates, Guardian admission/extraction, and new learned commits while preserving explicit correction/export/delete/forget governance. |
| A16 | Public service protocol | implemented | The latest UI messages are owned by `sylvander-protocol`, shared by Unix/WebSocket/TUI, and generated as JSON Schema. Unknown/old versions and shapes fail closed. External Channels receive subscribe-only bus access; authenticated chat and interactive controls enter through Runtime-owned UI operations. A new chat subscribes before exactly one publish, and any creation, metadata, Engine, subscription, or dispatch failure compensates durable session, Engine attachment, and AgentRun authority without deleting an existing session. |
| A17 | Session persistence | implemented | SQLite session schema version 1 persists sessions, messages, usage, archive/fork/compaction, sparse overrides, immutable Agent/Provider/Model revision pins, effective prompt/permissions/workspaces/executor, prompt manifest, and channel ownership metadata. Runtime shares the physical `sessions.db` file with the registry while preserving disjoint ownership: each component exact-validates its SQL and foreign keys and accepts only the companion's complete current object-name allowlist. Fresh boot and restart preserve the exact union; standalone, unknown, partial, obsolete, and damaged layouts fail closed. Execution revalidates durable state before provider or tool work. |
| A18 | Identity and authorization | implemented | Protocol-owned authenticated transport principals, default-deny Agent access, session ownership, per-operation policy, boundary limits, typed denials, and content-free denial audit are enforced. Runtime owns the latest-only stable `UserId`/`PrincipalBinding` store, external digest key, exact trusted-issuer policy, two-sided single-use link proof, and monotonic unlink/relink CAS. The owner-free identity subprotocol is available through the common UI protocol over Unix and WebSocket; each transport derives the sealed transport/instance/principal envelope from its authenticated connection. Linked principals automatically resolve to the stable user for profiles, sessions, and ordinary ingress, while unlinked principals receive deterministic domain-scoped identities. |
| A19 | DingTalk instances | implemented | Configuration supports multiple credential-isolated bots; sender/conversation mappings, ownership, authorization, Agent-scoped outbound subscriptions, Runtime supervision, replay protection, interactive approval/answer/interrupt controls, bounded delivery retry, and frame limits are instance-scoped and tested. |
| A20 | Telegram instances | implemented | Configured bots use the shared durable store, required webhook authentication, instance-scoped principals/chat mappings, authorization, replay protection, Agent-scoped outbound subscriptions, Runtime supervision, interactive approval/answer/interrupt controls, Unicode-safe chunking/truncation, bounded delivery retry, and request limits. |
| A21 | Other channels | implemented for current adapters | The production server constructs Unix, HTTP, WebSocket, DingTalk, Telegram, and WeChat under the common instance/authentication/supervision contract. Unix and WebSocket expose the complete typed UI protocol; HTTP exposes bounded authenticated chat ingress. WeChat decrypts and verifies enterprise callbacks, routes authenticated chat/control operations, and sends bounded completed replies and tool/control status through the active message API with renewable credentials and access-token refresh. |
| A22 | Channel supervision | implemented | Stable instance registrations carry bounded configuration-driven restart policy and channel workspace defaults. Runtime provides readiness, startup rollback, content-free instance health, exponential restart/backoff, failure isolation, cooperative drain, and terminal instance-ID reporting. Adapter startup and subscription failures return to supervision rather than panicking. |
| A23 | Run evidence | implemented | The durable run ledger correlates runs, turns, steps, outcomes, usage, tool activity, recovery, retention, queries, feedback, and content-free administration/authorization audit. Raw/redacted content and generated artifacts use the separately encrypted governed-record path. |
| A24 | Feedback | implemented | Typed positive/negative feedback records bounded corrections, task result, artifact/validation references, privacy class, and Runtime-derived immutable principal/channel/transport attribution. It is persisted only when it references a real evidence run and, optionally, a turn belonging to that run. |
| A25 | Self-improvement | implemented | Privacy-scoped deterministic cohort selection reports a stable digest, failure taxonomy, success/feedback rates, exact token and complete-cost truth, latency distribution, tool/approval/retry/timeout metrics, and explicit bias/completeness warnings. The immutable evaluation registry adds versioned scoring adapters, digest-pinned fixture/held-out datasets, baselines, thresholds, and server-recomputed complete comparison. Evidence-linked proposals require benefit/risk/components/rollback/evaluations and use an attributed optimistic review lifecycle. Local self-change runs in an isolated Git worktree, records HMAC-signed baseline/candidate/observation evidence, commits the exact evaluated candidate before a distinct human merge gate, merges with reversible history, and automatically reverts an observed threshold regression. |
| A26 | Data governance | implemented | One latest-only governed-record path covers content-bearing run events and generated artifacts. It provides structural JSON redaction, explicit classification, database-bound tenant/key identity, exact user scope, AES-256-GCM content encryption, finite startup/maintenance retention, all-or-nothing audited export/delete, physical ciphertext deletion, and non-reusable tombstones. Queryable classification/scope/audit metadata requires encrypted host storage when metadata-at-rest secrecy is also required. |
| A27 | Secrets | implemented for the current adapters | Typed environment/file references, bounded zeroizing values, immutable registry generations, activation preflight, and redacted administration are implemented. Provider requests use renewable external leases through a Runtime injection boundary; the built-in environment/file source re-resolves rotating values without rebuilding Agents. HTTP, WebSocket, DingTalk, Telegram, and WeChat acquire instance-scoped atomic credential bundles at authentication, connection, token-refresh, or delivery boundaries. Generation changes invalidate caches, expiry/renewal failure fails closed, and no adapter falls back to a previous or foreign instance's credential. The live Provider/Channel paths append create/acquire/renew/rotate/revoke/failure facts to a separate exact-schema, 90-day credential-operation ledger that accepts no secret or secret reference and isolates queries by stable subject. |
| A28 | Schema lifecycle | implemented under latest-only policy | Session schema version 1, the current registry component/V3 snapshot schema, and relationship memory have explicit version contracts. The shared session/registry file permits only their exact current two-owner namespace union while each owner exact-validates its own definitions; standalone, old, future, unmanaged, partial, duplicated, or damaged layouts fail startup without mutation, repair, downgrade, or fallback. Candidate configuration/registry activation is retry-safe and does not move the active head on failure. No compatibility migration is added without an explicitly approved source version and transition. |
| A29 | Shutdown and recovery | implemented | Runtime boot restores durable sessions, exact revision pins/manifests, interrupted evidence, maintenance state, and local or remote worktree leases. It removes or reconciles deleted-session and pre-manifest worktree orphans. Channel startup rollback, bounded restart/backoff, cooperative channel/Agent/evidence/maintenance drain, signed memory backup/restore, reviewed-merge revert, and repeatable recovery drills are documented and tested. |
| A30 | Observability and operations | implemented | Runtime exposes one content-safe operational snapshot with dependency readiness, Agent/session counts, per-instance channel health, evidence counts, and bounded-bus capacity/publish/backpressure counters. The HTTP adapter serves dependency-aware `/health`, `/ready`, and Prometheus `/metrics`; Server tracing supports human or flattened JSON export. Ingress size/rate quotas, bounded queues, restart policy, alert conditions, incident triage, recovery, and shutdown are documented in `operations-runbook.md`. |
| A31 | Concurrency isolation | implemented | Agent turns use per-session locks and active-turn cancellation; real-runtime PTY tests cover multi-client session isolation. This must remain a regression gate. |
| A32 | Approval authority | implemented | The Agent owns approval decisions and freezes one immutable capability surface per turn. Persistent grants require a Runtime-authenticated stable identity and bind exact `UserId`, `AgentId`, content-addressed policy revision, content-addressed capability revision, operation, and content-safe resource fingerprint. Any dimension change invalidates the grant. The current durable schema is atomic, bounded, fail-closed, and latest-only; transports can relay only Agent-advertised scopes. |
| A33 | Mutation recovery | implemented for supported mutation modes | Local non-Git Write/Edit operations use the conflict-checked workspace journal. Every mutable Git coding session uses a durable local/host-backed or SSH-native worktree transaction with inspect, accept, discard, compensation, and restart reconciliation. A writable remote non-Git workspace is rejected before session creation instead of running without an executor-neutral rollback boundary. |
| A34 | Test depth | implemented with deployment prerequisites | The checked-in suites cover Agent, protocol, TUI, PTY, real-runtime, executor conformance, multi-instance channels, crash/restart, worktrees, Guardian, governance, and self-improvement. The disposable local-SSH journey covers execution, remote cancellation, restart, review, accept, and discard. Real OCI, native tmux, credentialed Provider/external-channel, and deployment signing/notarization journeys require their host service or private credential and remain explicit deployment gates when those prerequisites are available. |

Audit boundary: approval authority is one typed stage of the unified
actor-aware invocation path. Runtime freezes the executable catalog, derives
the Worker owner from `ToolContext`, re-authorizes every built-in, MCP,
browser, host-control, memory-candidate, control, and extension route, and
records pre-execution plus terminal content-safe audit. Skills are frozen into
the same turn revision as prompt-only context and grant no executable
authority. Large MCP output uses the governed artifact sink rather than
unbounded audit or transcript content.

## 5. Prioritized executable backlog

Items are completed in order unless a later test-only item can run safely in
parallel. An item becomes `done` only when its acceptance evidence is linked.

### P0 — Correctness, safety, and durable contracts

- [x] **P0.1 Configuration schema and loader:** versioned server config,
  Agent definitions, model providers, execution targets, channel instances,
  secret references, validation, and redacted inspection.
  Evidence: `sylvander-runtime/src/config`, `config/sylvander.example.toml`, and
  `docs/server-configuration.md`; runtime tests cover validation, secrets,
  the maintained example, composition, and durable restart. `SYLVANDER_CONFIG`
  is mandatory; old/unknown schemas fail before boot.
- [x] **P0.2 Production composition root:** make `sylvander-runtime` the only
  boot path; use the configured durable store; supervise Agents and channels;
  graceful drain and explicit startup failures.
  Evidence: runtime-owned task handles, readiness handshakes, transactional
  channel startup, unexpected Agent/channel exit reporting, bounded cooperative
  drain, and a real Unix/HTTP server startup-health-shutdown smoke test.
- [x] **P0.3 Session effective configuration:** persist Agent revision, model,
  reasoning, permissions, prompt profile, workspaces, executor, and override
  provenance; require immutable component pins plus prompt manifest; snapshot
  atomically per turn.
  Evidence: protocol-owned sparse/effective/provenance types; dedicated SQLite
  columns and immutable turn snapshots; runtime resolution across Agent,
  channel, and session layers; optimistic revision updates; current-schema
  restart validation; and an end-to-end model request proving that the persisted
  model/prompt/permission selection is used before the provider or tools run.
  The configured execution-target identity is durable here; backend-neutral
  filesystem/process execution remains deliberately tracked by P3.1/P3.2.
- [x] **P0.4 Public protocol v5:** move service messages into
  `sylvander-protocol`; add Agent discovery, session create/update/effective
  state, feedback, and optimistic concurrency; generate and strictly test the
  current schema. Evidence: one shared `UiClientMessage`/`UiServerMessage` contract
  across Unix, WebSocket, and TUI; runtime-owned `UiService`; durable configured
  session creation and optimistic updates; evidence-linked feedback; and
  generated JSON Schema with old/unknown-shape rejection.
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
  - [x] Public and durable session selection is provider-qualified; bare model
    ids are rejected.
  - [x] Production `AgentLoop` uses the provider-neutral backend. Provider
    streams are checked for one terminal completion and exact model identity.
    Manual and automatic compaction use the same pinned provider backend and
    return typed, redacted failures.
  - [x] Extend the existing `sessions.db` Agent registry SSOT with component
    ledgers and immutable Provider/Model/Credential revision tables. Do not
    create a second registry database.
    Evidence: the current component ledger plus integrity-checked registry
    domain loaders in `sylvander-runtime/src/agent_registry.rs` and
    `sylvander-runtime/src/registry_domain.rs`.
  - [x] Add true SQL compare-and-swap across multiple registry connections,
    integrity validation, current-schema restart checks, lifecycle, and pricing metadata.
    Provider/Model/Credential heads use optimistic SQL updates and immutable
    digest-checked definitions in the existing `sessions.db` SSOT.
  - [x] Route active Provider/Model revisions dynamically while sessions pin
    definition revisions; credential bindings rotate live by generation and
    never persist resolved secret values.
    Evidence: immutable Agent registry snapshots, exact production
    `RuntimeRevisionProvider` composition, persisted Provider/Model session
    pins, current-schema restart validation, execution-boundary revalidation,
    and request-scoped credential rotation tests in `sylvander-runtime`.
    The latest-only registry physically contains the base catalog and V3
    snapshot tables only. V2 snapshot tables, APIs, loaders, and composition
    paths do not ship; an external old or mixed database fails the exact schema
    check without being upgraded or mutated.
  - [x] Expose redacted Provider/Model/Credential revision inspection through
    the public protocol with transport authorization, service authorization,
    immutable exact-version reads, bounded database pagination, and durable
    content-free audit.
  - [x] Expose Credential create/stage/activate/rollback with strict immutable
    generations, optimistic head concurrency, exact-generation availability
    preflight, typed redacted failures, durable pre-mutation intent and terminal
    audit, UI protocol v5 negotiation, and Unix/WebSocket round trips.
  - [x] Expose Provider/Model create/stage/activate/rollback through strict UI
    protocol v5 drafts with typed redacted failures, SQL compare-and-swap,
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
    RegistryAdmin and AgentAdmin adoption, DiscoverAgents metadata, bare or
    ambiguous selection rejection before mutation, exact session revision pins,
    restart restoration, live Credential rotation, and one-provider failure
    without fallback or contamination of a healthy Provider.
- [x] **P1.3 Prompt resolver:** shared safety layers, exact qualified
  model/provider profiles, Agent prompt, allowed write-only session input,
  ordered provenance/digests, byte limits, deterministic restart, and
  execution-boundary tamper rejection. Evidence: `sylvander-agent/src/prompt.rs`,
  `sylvander-agent/src/run.rs`, protocol schema/redaction tests, real Unix and
  WebSocket response tests, provider-wire composition tests, and runtime
  restart acceptance in `registry_agent_composition_tests.rs`.
- [x] **P1.4 Durable memory:** durable Agent-owned memory lifecycle and
  governance.
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
  - [x] Bound audit and retention-ledger growth only after a verified external
    checkpoint preserves the evidence required for recovery and inspection.
  - [x] Add a remote monotonic CAS anchor backend for deployments whose threat
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
    Timeouts are bounded to 100–30000 ms and read retries to at most 3. CAS
    conflicts and ambiguous writes fail closed rather than being converted
    into blind retries. This backend is the required deployment shape when the
    threat model includes whole-host historical replay.
  - Mutations use a two-phase anchor transition: a signed
    `Pending{from_epoch/root,to_epoch/root}` is fsynced before SQLite commit,
    then finalized after commit. Restart accepts only the authenticated
    `from` root (transaction rolled back) or `to` root (transaction committed)
    and deterministically repairs the anchor; every third state fails closed.
    Read-only recall never scans the full database or rewrites the anchor.
  - Schema v7 stores an epoch-bound HMAC for every model-visible memory row and
    an externally anchored, replaceable retention-policy stage. Policy
    activation is CAS-bound to the active base revision and happens only after
    Runtime readiness succeeds; failed rollout stages never reserve a revision.
    Insert, update, supersede, delete, and maintenance transactions re-seal
    rows before prepare/commit. `get` and `search` verify only returned rows
    against the committed anchor epoch, so an online database writer cannot
    feed forged or replayed row content to the model between checkpoints.
  - A compaction checkpoint is a signed backup whose epoch/root still equals
    the current external anchor when the SQLite `IMMEDIATE` transaction holds
    the writer lock. Each finite batch retains the newest audit and retention
    boundary rows and folds deleted rows into cumulative, domain-separated
    roots in one constant-size checkpoint state. The state, counts, backup
    digest, and prior roots are covered by the next anchor root. Ordinary SQL
    lacks the thread-scoped maintenance gate required by exact-schema delete
    and checkpoint triggers. A failed batch commits neither deletes nor state;
    a successful batch is followed by a new verified backup before another
    batch or return, preserving an artifact at the final anchored epoch.

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
  writer serialization), and `63d9b6e` plus `d0bfec0` (checkpoint-authorized
  bounded evidence chains and Runtime publication of the final restorable
  epoch). The current acceptance suite covers every P1.4 gate above.
- [x] **P1.5 Stable user identity and account binding:** Runtime owns the
  latest-only stable user/principal store and its external HMAC key. Channel
  ingress derives typed external principals only after platform
  authentication; public clients and models cannot self-assert one or access
  the store. Expose explicit, expiring, single-use begin/confirm and
  owner-authorized CAS unlink operations through the versioned UI service.
  Storage-domain evidence is covered by `367214999` through `e86abd1a2`.
  Runtime composition proves two-sided linking, cross-channel profile/session
  ownership, and stable-user persistence. Unix and WebSocket expose the same
  owner-free request/response envelope, advertise the negotiated capability,
  and test that the sealed identity comes only from authenticated ingress.
- [x] **P1.6 Optional Provider catalog synchronization:** let adapters that
  expose a remote model catalog enumerate it, reconcile discovered metadata
  against the Registry SSOT, report drift and health, and never silently
  rewrite an active Agent snapshot. Providers without a reliable enumeration
  contract continue to use validated operator-managed Registry metadata.

### P2 — Workspace and extension platform

- [x] **P2.1 Workspace composition:** Agent home plus task/dependency/artifact
  mounts, logical references, capability policy, collision rules, and effective
  workspace inspection.

  Current evidence: the effective session configuration now carries the
  canonical role-bearing mount set. Agent home and task bindings become
  `@agent` and `@task`; configured dependency and artifact mounts retain their
  declared logical references and independent read/write/command/Git policy.
  Invalid references, duplicate references, ambiguous dependency/artifact
  target-path overlaps, and read-only capability conflicts fail before a turn
  can execute; Agent home and task may intentionally alias one location. Public
  Agent administration preserves mount definitions and redacted inspection
  reports their count. Read/Write/Edit/List/Search route `@reference/path`;
  Command and Git accept an explicit workspace reference. The turn prompt
  lists only logical names, roles, and allowed operations so the model can use
  the composition without depending on backend paths.
- [x] **P2.2 AGENTS.md resolver:** hierarchical discovery across Agent/task
  workspaces, precedence, aliases, size limits, provenance, cache invalidation,
  and prompt integration.

  Agent-home instructions precede task instructions. Within either binding,
  the resolver walks from the workspace root to its relative
  `instruction_focus`; later, more specific documents win. `AGENTS.md`,
  `AGENT.md`, and `agent.md` have deterministic alias priority. Every accepted
  document is bounded, path-attributed in the prompt, reloaded per turn, and
  cannot escape the executor-backed workspace root.
- [x] **P2.3 Skills runtime:** package format, discovery, trust, activation,
  instruction/resource loading, validation, health, and protocol inspection.

  Agent-home and task-workspace packages use a strict optional `SKILL.toml`
  manifest plus mandatory `SKILL.md`. The manifest controls activation and
  exact relative resources. Invalid, incomplete, oversized, or disabled
  packages never inject partial content. Active/configured/degraded health,
  trust, provenance, capabilities, and per-turn reload truth are available
  through the redacted platform snapshot.
- [x] **P2.4 MCP runtime:** supervised transports, initialization, tool/resource
  discovery, namespacing, auth references, timeouts, cancellation, restart,
  health, and safe tool adaptation.

  The local production transport is MCP 2025-11-25 over stdio. Runtime-owned
  clients resolve secret references, supervise and drain child processes,
  publish bounded namespaced tool/resource adapters, send protocol
  cancellation on timeout and dropped requests, reconnect without replay,
  refresh catalogs atomically, send complete results to the Runtime-owned
  encrypted artifact sink, and expose redacted health counters. See
  [`sylvander-agent/docs/mcp.md`](../sylvander-agent/docs/mcp.md).

### P3 — Location-transparent execution and coding isolation

- [x] **P3.1 Executor contract:** local reference implementation plus a shared
  conformance suite for filesystem, process, environment, cancellation,
  streaming, limits, and Git operations.
- [x] **P3.2 Executor-backed tools:** migrate Read/Write/Edit and add bounded
  List/Search/Command/Git operations without exposing backend location to the
  Agent loop.

  Current evidence: the first local slice provides `WorkspaceExecutor`,
  `WorkspaceTarget`, and `LocalExecutor`; Read/Write/Edit/Command delegate to
  it, session workspace bindings select the target per turn, read-only is
  enforced as the intersection of workspace and permission policy, configured
  local roots bound accepted workspaces, and unknown adapters never execute
  against a same-named host path. The OpenSSH adapter uses fixed batch-mode
  arguments, bounded operations, remote workspace-relative file access, and
  stdin-separated file/command payloads. Structured List/Search and read-only
  Git status/diff/log now use the same executor boundary and return explicitly
  bounded results. Local and SSH command execution concurrently drains both
  output streams, keeps only a fixed head/tail capture, records exact byte
  totals and truncation, and returns a compact structured result to the model
  and TUI. Live command progress now crosses the existing tool-delta protocol
  through a bounded Agent queue. The collapsed TUI shows only the latest useful
  line; expanded details remain bounded and favor the error-bearing tail.
  Dropping an executor operation is the explicit cancellation boundary. Local
  commands run in an isolated process group and synchronously terminate that
  group on timeout, interrupt, or future cancellation; tests use a background
  descendant that attempts a delayed filesystem side effect and prove it
  cannot survive. SSH transport processes use kill-on-drop, while remote
  process-group cancellation is implemented by the remote wrapper and covered
  by the opt-in real-SSH acceptance journey.
  The first container adapter now runs every operation in a disposable,
  network-disabled container with a bounded bind mount, read-only inspection,
  live bounded command output, and daemon-side forced cleanup on cancellation.
  A Runtime-level dogfood journey now creates a clean-Git session worktree,
  edits it through the container executor, inspects and accepts the diff,
  restarts Runtime with the same durable lease, performs another container
  edit, and discards the session without changing the accepted source state.
  Container and managed-sandbox targets now apply validated memory, CPU, and
  process ceilings plus a read-only root filesystem, private bounded `/tmp`,
  dropped capabilities, and `no-new-privileges`. A sandbox profile is an
  OCI-compatible driver plus an immutable image reference. Environments remain
  deliberately disposable: durable coding state belongs in the isolated host
  worktree, while reusing an execution container would leak state across
  operations. SSH pooling, explicit host-key policy, remote worktree review,
  restart reconciliation, accept, and discard are implemented.

  The local environment contract is now explicit: Command accepts bounded,
  validated per-invocation overrides, Local applies them only to the child
  process, unsupported adapters reject rather than ignore them, and ordinary
  plus streaming paths share the same behavior. A reusable conformance helper
  exercises filesystem, bounded query, environment, process, streaming, and
  read-only inspection behavior through Local and the logical mount router.
  Detailed invariants are in
  [`sylvander-agent/docs/workspace-execution.md`](../sylvander-agent/docs/workspace-execution.md).
- [x] **P3.3 OpenSSH execution:** configured SSH targets resolve an identity
  path through the secret boundary, require a deployment-owned known-hosts
  file with strict verification, reuse bounded OpenSSH control connections,
  validate remote workspace paths, bound both output streams, publish live
  progress, and terminate the owned remote process group on timeout,
  interruption, or dropped futures. Unit tests use a deterministic fake
  transport; the credentialed real-SSH journey is an opt-in deployment gate.
- [x] **P3.4 Container and sandbox executors:** disposable OCI operations have
  bounded mounts, network denial, read-only root filesystems, private temporary
  storage, dropped capabilities, no-new-privileges, validated resource
  ceilings, cleanup, conformance tests, managed-sandbox composition, and the
  complete host-backed coding-session journey across a Runtime restart.
  Reusable environments are intentionally excluded because durable state lives
  in isolated worktrees and executor reuse would create cross-operation state.
- [x] **P3.5 Worktree managers:** writable Git coding sessions default to
  collision-free durable leases. Local/host-backed and SSH managers implement
  review, merge, abandon, restart, and compensation paths. Runtime boot
  validates active leases against the durable effective workspace,
  garbage-collects deleted-session leases, and reconciles remote worktrees
  through local intent manifests plus the configured SSH executor.

### P4 — Multi-instance channels

- [x] **P4.1 Channel registry and supervisor:** typed instance configuration,
  composite external identity, scoped bus subscriptions, health/restart/drain,
  deduplication, and route policy.
- [x] **P4.2 DingTalk production adapter:** multiple bots, instance-isolated
  sessions/credentials, interactive decisions, retries, limits, and tests.
- [x] **P4.3 Telegram production adapter:** remove the private store, support
  multiple webhook instances/routes, verify webhook authenticity, interactive
  decisions, Unicode-safe chunking, retries, limits, and tests.
- [x] **P4.4 Remaining adapters:** apply the same instance/auth/supervision
  contract to Unix, HTTP, WebSocket, and WeChat without weakening the public
  protocol.

### P5 — Evidence-driven self-improvement

- [x] **P5.1 Feedback and outcome APIs:** user ratings/corrections, task result,
  artifact and validation links, privacy class, and immutable attribution.
- [x] **P5.2 Analysis pipeline:** reproducible cohorts, failure taxonomy,
  quality/cost/latency/tool/approval/recovery metrics, and bias/leakage checks.
- [x] **P5.3 Evaluation registry:** versioned datasets, deterministic fixtures,
  held-out cases, scoring adapters, baselines, and regression thresholds.
- [x] **P5.4 Improvement proposal:** evidence-linked hypotheses with expected
  benefit, risk, affected components, rollback, and required evaluations.
- [x] **P5.5 Self-change experiment:** isolated worktree implementation,
  baseline/candidate execution, signed evidence bundle, human approval, merge,
  local post-merge observation, and automatic rollback criteria. A compiled
  administrator-binary journey covers successful observation and an explicit
  clean Git rollback; this is not a remote deployment claim.

### P6 — Production closure

- [x] **P6.1 Latest-schema and recovery drills:** fail-closed schema/config validation,
  interrupted upgrades, backup/restore, executor/channel restart, and orphan
  worktree/run recovery.
- [x] **P6.2 Operational controls:** health/readiness, metrics, tracing export,
  queue/backpressure limits, quotas, diagnostics, alerts, and runbooks.
- [x] **P6.3 Security verification:** the repeatable release gate covers the
  threat model, tracked-secret scan, locked RustSec dependency audit,
  deterministic malformed/mutated protocol parsing, path and command-argument
  injection, cross-owner isolation, credential redaction, and complete learned
  data deletion. Environment-dependent limitations are recorded in
  [`security-verification.md`](security-verification.md).
- [x] **P6.4 Local performance verification:** a locked release build and
  isolated repeatable gate cover concurrent message delivery, parallel tools,
  long TUI transcripts, large workspaces, tool/input/service bursts, executor
  resource ceilings, and explicit latency budgets. SSH and external-service
  latency are outside the current local scope and recorded in
  [`performance-verification.md`](performance-verification.md).
- [x] **P6.5 Final closure:** locked full-workspace tests and linting,
  clean-room release installation/startup/shutdown, compiled real-client
  journeys, security and performance gates, zero known critical/high defects,
  no unchecked current-scope item, and explicit residual risks are recorded in
  [`release-closure.md`](release-closure.md). The active
  [`production-expansion-checklist.md`](production-expansion-checklist.md) and
  its same-state release matrix are complete.

## 6. Verification policy

Every implementation batch must include:

1. unit tests for domain invariants and validation failures;
2. integration tests across the public boundary it changes;
3. persistence/restart tests for durable state;
4. negative security and cross-session/instance isolation tests;
5. formatting, clippy with warnings denied, workspace tests, and release build;
6. documentation and current-schema rollout/rollback notes in the same batch;
7. a reversible commit that does not mix unrelated subsystems.

Real credentials are never required for the default suite. Provider, SSH,
container, MCP, DingTalk, and Telegram contracts use deterministic local fakes;
credentialed smoke tests are opt-in and redact all captured data.
