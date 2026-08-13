# `sylvander-agent` architecture

`sylvander-agent` is the deterministic execution kernel for one bounded Agent
turn. It owns model/tool iteration and an in-memory
conversation snapshot, not the product Session that supplied that snapshot.
Runtime owns authentication, Session lifecycle and persistence, scheduling,
public stream events, and concrete infrastructure.

The normative target and migration rules are documented in
[`../../docs/agent-runtime-api-boundaries.md`](../../docs/agent-runtime-api-boundaries.md).
Runtime now owns `AgentRun`, supervision, Session persistence, public event
mapping, MCP process transport, and durable relationship-memory persistence.

## Physical module hierarchy

```text
Runtime Agent service
  -> turn data: AgentTurnRequest
  -> turn services: AgentExecutionPorts
  -> kernel policy and state machine: AgentLoop / run_stream
  -> domain subsystems: context, compression, tools, gates, neutral ports
  -> progress: AgentEvent
  -> result: AgentOutcome
```

```text
src/
  turn/          immutable turn vocabulary and authority
  kernel/        stable policy and model/tool iteration
  context/       prompt composition, retrieval, profiles, compression
  tool/          contracts, authorization, registry, built-ins
  execution/     Runtime-injected workspace, artifact, and mutation capabilities
  interaction/   approval, AskUser, plan, and background-task gates
  memory/        relationship-memory domain, retention, and storage ports
```

These directories are the internal source of truth. `lib.rs` is only the
external facade; it may re-export established API paths, but internal code uses
the owning physical namespace so dependencies remain visible during review.

`AgentTurnRequest` and `AgentExecutionPorts` are sibling inputs, not nested
service objects. The request freezes model-visible domain data; the ports
freeze Runtime-selected implementations and executable authority. The kernel
validates that both describe the same turn before opening a provider stream or
running a hook.

- `context::turn_context` composes the immutable Safety/Agent/User Profile/
  Relationship Memory/Workspace Knowledge/Session precedence chain. It applies
  per-layer byte, token-estimate, and item budgets and records content-safe
  provenance plus digests for every included item.
- `kernel::agent_loop` contains only stable execution policy and the provider-neutral
  model/tool state machine. `AgentLoop` does not retain provider, model,
  transcript, tools, workspace, or authority. Runtime freezes those values in
  `AgentTurnRequest` and `AgentExecutionPorts`; the loop validates that both
  snapshots describe the same executable surface before work starts.
- `tool` and `execution::tool_context` define the invocation boundary. Tools receive
  Runtime-derived identity, workspace, capability, and execution-budget data;
  model arguments are never authority.
- `ToolSpec::prompt_guidelines` lets a tool carry concise usage rules beside
  its neutral JSON Schema. The frozen registry emits guidelines only for
  immediately visible tools, sorted by stable tool name. Guidelines are part
  of the capability revision because changing model-visible operating rules
  changes the executable contract. Provider adapters do not own or rewrite
  these rules.
- `execution::workspace` contains only the neutral workspace port, values,
  router, bounds, and fail-closed unavailable sentinel. Concrete local, SSH,
  and OCI implementations plus cross-Session workspace coordination live in
  Runtime. Edit carries an opaque content revision from update-read to
  conditional write and fails closed when the environment cannot enforce that
  contract. `ToolContext` never grants host access merely because a filesystem
  path was supplied.
- `execution::artifact` defines the asynchronous, turn-bound retention port. Agent sends
  media type, bytes, and call correlation only; Runtime binds identity,
  encryption, retention, backend, and locator resolution. Compression exposes
  only an opaque `artifact:` locator and never a host path.
- `tool::builtins` contains Agent-owned prepared-call handlers. Concrete
  storage and process services are injected through the execution context.
  Relationship-memory domain values, validation, and the `MemoryStore` port
  live under `memory`; SQLite, integrity anchors,
  backup, restore, and maintenance live in Runtime.
- MCP is not part of the Agent source tree. Runtime owns the stdio process,
  JSON-RPC lifecycle, discovery, health, cancellation, and artifact sink, then
  registers MCP tools through the ordinary Agent tool contract.

## Invariants

1. Runtime issues its `AgentRun` with an authenticated session lease and then
   constructs Agent inputs. Raw bus metadata and client-provided identifiers
   cannot create trusted authority.
2. Each turn reads its effective Agent/model/workspace configuration from the
   durable session snapshot. A session override may be more specific than the
   Agent default but cannot select an unauthorized capability.
3. Approval, AskUser, plan, and task gates pause or constrain the current run;
   they never let model content forge a response under another session.
4. Every concrete tool call checks its `ToolContext` capability snapshot and
   records content-safe lifecycle evidence. Oversized plain text is replaced
   only after Runtime confirms durable artifact retention; failure preserves
   the only inline copy. Workspace tools are stateless and reject
   an empty context workspace instead of falling back to constructor or process
   paths.
5. Background tasks get a new explicit task prompt and a reduced read-only
   capability set. Runtime derives this set from stable built-in name
   constants and recomposes tool guidelines from the reduced registry, so a
   background prompt cannot advertise a parent-only mutating tool. Background
   tasks do not inherit private chain-of-thought or silently mutate the parent
   session.
6. Schema ownership remains explicit even when Runtime deliberately shares one
   SQLite file between the session store and Agent registry. Standalone open
   accepts only the exact session object set. Shared open accepts only the
   exact union of the session store's fixed namespace and the companion
   registry's complete current object-name allowlist; each component still
   exact-matches the SQL for every object it owns. Memory, profile, evidence,
   Guardian, extension, operator-created, undeclared, and obsolete objects are
   rejected. Runtime creates the session namespace first on an empty file so
   its application ID and `user_version = 1` remain the file-level contract,
   then atomically installs or validates the registry namespace.
7. Runtime pins an exact `(provider_id, model_id)` before building a turn.
   Agent execution never falls back to a direct client, an unqualified model
   name, or a second provider when the selected route fails.
8. `AgentOutcome` contains the updated conversation and cumulative usage.
   Runtime, not the Agent kernel, decides when that result becomes durable or
   visible through the public API.

## Extension points

- Implement a `ModelProvider` in a provider crate, then register it in Runtime.
- Add a built-in tool through the registry and require an explicit capability,
  input schema, execution budget, and tests for both allowed and denied paths.
- Add MCP or Skill support through their dedicated loaders; never inject
  unvalidated filesystem content directly into the system prompt.
- Use `WorkspaceExecutor` for file and command work so local, SSH, container,
  and managed-sandbox targets share one result contract.

## Verification

White-box tests live under `tests/unit/` and are linked through test-only path
bridges; public journeys live directly under `tests/`. The suite covers
authenticated run issuance, typed turn-context budgeting, provider conversion,
tool capability denial, approval/AskUser gates, workspace ports, in-memory
memory-contract behavior, Skills, compression, and cancellation. Runtime tests
cover durable memory, MCP, concrete executors, and Session restoration. Real
provider tests are explicitly ignored unless credentials are supplied.

```bash
cargo test -p sylvander-agent --all-targets --locked
cargo clippy -p sylvander-agent --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p sylvander-agent --no-deps --locked
```

## Related documentation

- [`execution-kernel.md`](execution-kernel.md) — Agent-owned turn vocabulary,
  construction, authority invariants, and code documentation rules.
- [`workspace-execution.md`](workspace-execution.md) — executor and coding tool
  rules.
- [`../../sylvander-runtime/docs/mcp.md`](../../sylvander-runtime/docs/mcp.md)
  — Runtime-owned MCP lifecycle and bounded result handling.
- [`skills.md`](skills.md) — Skill discovery and per-turn budget.
- [`approval.md`](approval.md) — stable-identity persistent approval keys,
  invalidation, and durable-store operations.
- [`turn-context.md`](turn-context.md) — typed precedence, relevance retrieval,
  provenance, and prompt budgets.
- [`../../docs/sylvander-agent-platform.md`](../../docs/sylvander-agent-platform.md)
  — Runtime-to-Agent architecture and product scope.
