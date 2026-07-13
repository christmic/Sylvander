# Sylvander Server Agent Platform

Status: normative architecture and production backlog

Last audited: 2026-07-14

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
their recorded revision until explicitly migrated or resumed under a declared
migration policy.

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
| A01 | Agent specification | partial | `AgentSpec` serializes identity, one prompt, one model, memory declarations, tools, and MCP declarations. The production server does not load it from configuration. |
| A02 | Agent registry | missing | The server constructs one hard-coded `assistant`; there is no durable definition registry, revision, validation command, or reload path. |
| A03 | Runtime composition | defect | `sylvander-server` bypasses `sylvander-runtime` and builds `AgentRun` directly. The runtime itself opens an in-memory SQLite store, so it is not the production composition root it claims to be. |
| A04 | Session model override | defect | `AgentRun::select_model` mutates one `RuntimeModels.current_model`; all sessions share it and the selection is not stored with the session. |
| A05 | Session permission override | defect | Runtime permissions are also Agent-global rather than session-scoped. One client can change later turns in other sessions. |
| A06 | Model providers | partial | A catalog of model ids exists, but all selections use one `AnthropicClient`; `ModelConfig.provider` does not resolve a provider implementation. |
| A07 | Model-specific prompts | missing | One `persona.system_prompt` is copied into every loop. There is no provider/model profile, prompt provenance, or safe session override. |
| A08 | Agent workspace | missing | Only `SessionMetadata.workspace` exists. Agent home, multiple mounts, roles, and workspace composition are absent. |
| A09 | File tools | partial | Read/Write/Edit enforce capabilities and a canonical local root, but call `std::fs` directly and cannot address remote/container/sandbox resources. |
| A10 | Command/Git tools | missing | The Agent has no production spawn/shell/Git tool surface. `Cap::Spawn` and `Cap::Git` are declarations without executor-backed tools. |
| A11 | Worktree isolation | missing | No worktree lease, branch lifecycle, merge gate, ownership, or cleanup service exists. |
| A12 | AGENTS.md | missing | Repository guides exist for developers, but the running Agent does not discover or assemble workspace instructions. |
| A13 | Skills | missing | Protocol/UI placeholders can display Skills, but the Agent has no Skill discovery, trust, activation, or instruction loading runtime. |
| A14 | MCP | defect | MCP configuration types and UI inspection exist, but no MCP process/client, discovery, execution, health, or resource implementation exists. The UI correctly reports configuration only. |
| A15 | Agent memory | defect | The server injects `InMemoryMemoryStore`; `MemoryStoreConfig` says SQLite is planned and rejects it. Agent memory is lost on restart. |
| A16 | Public service protocol | defect | UI hello/version types live in `sylvander-protocol`, but the actual Unix `ClientMsg`/`ServerMsg` contract is private to the Unix crate. Schema/code generation described by the protocol crate is disabled. |
| A17 | Session persistence | partial | SQLite persists sessions, messages, usage, archive, fork, and compaction. It does not persist effective runtime overrides, Agent revision, executor, mounts, worktree, or channel instance binding. |
| A18 | Identity and authorization | partial | Session context carries user/Agent/session identity and store queries support scoping, but client/channel authentication and an authorization policy are not consistently enforced at the service boundary. |
| A19 | DingTalk instances | defect | One optional environment-configured DingTalk bot is started. Session keys use conversation id without a channel-instance namespace. |
| A20 | Telegram instances | defect | The server does not start Telegram. The channel creates a private in-memory session store for inbound mapping instead of using `ChannelContext.sessions`; chat ids are not instance-namespaced. |
| A21 | Other channels | partial | HTTP, Unix, WebSocket, Telegram, and WeChat crates exist, but the production server starts only HTTP, Unix, and at most one DingTalk instance. |
| A22 | Channel supervision | partial | DingTalk reconnects internally, but runtime-wide instance health, restart backoff, readiness, drain, and failure isolation are not modeled. Some channel startup paths unwrap. |
| A23 | Run evidence | partial | Tracing spans, persisted messages, aggregate usage, tool stream events, and workspace journal records exist. There is no durable correlated run/turn/step ledger or outcome model. |
| A24 | Feedback | partial | Approval rejection text exists, but no general feedback protocol/store ties user assessment to a completed run or artifact. |
| A25 | Self-improvement | missing | There is no evidence selection, evaluation corpus, proposal, experiment, comparison, or human merge gate. |
| A26 | Data governance | missing | Run-data classification, redaction, encryption, retention, deletion, export, and cross-tenant isolation policy are not implemented. |
| A27 | Secrets | partial | Core credentials come from environment variables and debug formatting avoids the API key. There is no secret-reference abstraction, rotation, or per-channel isolation. |
| A28 | Database migrations | partial | SQLite uses create-if-missing and targeted column checks. There is no explicit schema version, ordered migration ledger, backup/restore verification, or downgrade policy. |
| A29 | Shutdown and recovery | partial | Sessions can recover and turns can be interrupted, but server shutdown aborts tasks rather than draining channels/turns and closing durable terminal states. |
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
- [ ] **P0.2 Production composition root:** make `sylvander-runtime` the only
  boot path; use the configured durable store; supervise Agents and channels;
  graceful drain and explicit startup failures.
- [ ] **P0.3 Session effective configuration:** persist Agent revision, model,
  reasoning, permissions, prompt profile, workspaces, executor, and override
  provenance; snapshot atomically per turn; migrate existing sessions.
- [ ] **P0.4 Public protocol v2:** move service messages into
  `sylvander-protocol`; add Agent discovery, session create/update/effective
  state, feedback, and optimistic concurrency; generate and compatibility-test
  the schema.
- [ ] **P0.5 Boundary authorization:** authenticated principals, Agent/session
  ownership, channel-instance identity, policy checks, safe defaults, rate and
  payload limits, and auditable denials.
- [ ] **P0.6 Run ledger foundation:** durable run/turn/step/outcome records,
  correlation, crash recovery, redaction, retention, and query APIs before new
  execution backends multiply evidence sources.

### P1 — Persistent Agent organism

- [ ] **P1.1 Agent registry and revisions:** load, validate, persist, inspect,
  update, activate, roll back, and bind sessions to revisions.
- [ ] **P1.2 Provider and model registry:** provider-neutral client factory,
  capability discovery/validation, per-Agent default, per-session override,
  lifecycle, pricing, and credential references.
- [ ] **P1.3 Prompt resolver:** shared safety layers, model/provider profiles,
  Agent prompt, allowed session input, provenance/digests, limits, and tests.
- [ ] **P1.4 Durable memory:** Agent-namespaced SQLite memory, retrieval/write
  policy, provenance, retention/deletion, backup, and migration from configured
  stores.

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
