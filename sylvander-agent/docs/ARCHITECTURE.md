# `sylvander-agent` architecture

`sylvander-agent` is being reduced to the deterministic execution kernel for
one bounded Agent turn. It owns model/tool iteration and an in-memory
conversation snapshot, not the product Session that supplied that snapshot.
Runtime owns authentication, Session lifecycle and persistence, scheduling,
public stream events, and concrete infrastructure.

The normative target and migration rules are documented in
[`../../docs/agent-runtime-api-boundaries.md`](../../docs/agent-runtime-api-boundaries.md).
Runtime now owns `AgentRun`, supervision, Session persistence, public event
mapping, MCP process transport, and durable relationship-memory persistence.
The remaining bus, concrete local workspace/process implementation, and
product-Protocol dependencies in this crate are migration debt, not the
intended boundary.

## Internal layers

```text
Runtime Agent service
  -> immutable AgentTurnRequest + conversation snapshot
  -> provider-neutral Agent execution kernel
  -> ToolRegistry / ToolContext / approval & AskUser gates
  -> injected execution ports
  -> AgentEvent + AgentOutcome
```

- `turn_context` composes the immutable Safety/Agent/User Profile/
  Relationship Memory/Workspace Knowledge/Session precedence chain. It applies
  per-layer byte, token-estimate, and item budgets and records content-safe
  provenance plus digests for every included item.
- `engine` serializes work per session and exposes run lifecycle to Runtime.
- `loop_` contains only stable execution policy and the provider-neutral
  model/tool state machine. `AgentLoop` does not retain provider, model,
  transcript, tools, workspace, or authority. Runtime freezes those values in
  `AgentTurnRequest` and `AgentExecutionPorts`; the loop validates that both
  snapshots describe the same executable surface before work starts.
- `tool` and `tool_context` define the invocation boundary. Tools receive
  Runtime-derived identity, workspace, capability, and execution-budget data;
  model arguments are never authority.
- `workspace_executor` currently contains the neutral workspace port and a
  legacy local implementation. The port remains Agent-owned; concrete local,
  SSH, and OCI implementations belong to Runtime.
- `tools` contains Agent-owned definitions and prepared-call handlers. Concrete
  storage and process services are injected through the execution context.
  Relationship-memory domain values, validation, and the `MemoryStore` port
  remain here because tools consume that contract; SQLite, integrity anchors,
  backup, restore, and maintenance live in Runtime.
- MCP is not part of the Agent source tree. Runtime owns the stdio process,
  JSON-RPC lifecycle, discovery, health, cancellation, and artifact sink, then
  registers MCP tools through the ordinary Agent tool contract.

## Invariants

1. `AgentRun` is issued by Runtime with an authenticated session lease. Raw bus
   metadata and client-provided identifiers cannot create trusted authority.
2. Each turn reads its effective Agent/model/workspace configuration from the
   durable session snapshot. A session override may be more specific than the
   Agent default but cannot select an unauthorized capability.
3. Approval, AskUser, plan, and task gates pause or constrain the current run;
   they never let model content forge a response under another session.
4. Every concrete tool call checks its `ToolContext` capability snapshot and
   records content-safe lifecycle evidence. Tool output is size-bounded before
   returning to the model or a client. Workspace tools are stateless and reject
   an empty context workspace instead of falling back to constructor or process
   paths.
5. Background tasks get a new explicit task prompt and a reduced read-only
   capability set. They do not inherit private chain-of-thought or silently
   mutate the parent session.
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
