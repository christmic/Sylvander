# `sylvander-agent` architecture

`sylvander-agent` owns the deterministic per-session Agent execution loop. It
turns an authenticated Runtime-issued session into model requests, tool calls,
stream events, durable transcript entries, and bounded background work. It is
not a server composition root and it does not expose network listeners.

## Internal layers

```text
AgentRun / AgentRunEngine
  -> prompt composition + session snapshot
  -> provider-neutral AgentLoop
  -> ToolRegistry / ToolContext / approval & AskUser gates
  -> workspace executor, Skills, MCP, memory
  -> StreamEvent + durable session history
```

- `run` owns a single authenticated turn, cancellation, transcript persistence,
  prompt construction, and tool-result re-feeding.
- `turn_context` composes the immutable Safety/Agent/User Profile/
  Relationship Memory/Workspace Knowledge/Session precedence chain. It applies
  per-layer byte, token-estimate, and item budgets and records content-safe
  provenance plus digests for every included item.
- `engine` serializes work per session and exposes run lifecycle to Runtime.
- `loop_` exposes only provider-qualified production composition:
  `qualified_router` plus exact `provider_model` metadata. Direct-client and
  unqualified constructors do not exist. Standalone builders must also provide
  an explicit `ToolContext`; no model-derived user/session placeholder exists.
  The internal
  Anthropic wire adapter translates transcript/tool shapes without becoming a
  second route or fallback.
- `tool` and `tool_context` define the invocation boundary. Tools receive
  Runtime-derived identity, workspace, capability, and execution-budget data;
  model arguments are never authority.
- `workspace_executor`, `tools`, `mcp_stdio`, and skill loading are adapters
  below the Tool boundary. They return bounded structured results and artifacts
  rather than unbounded transcript text.
- `session_store` is the durable transcript/config store. Production uses the
  SQLite implementation injected by Runtime. A completely empty database is
  initialized directly at session schema version 1; every existing database
  must match the Sylvander session application ID, `user_version`, complete
  table/index definition set, foreign-key rules, and SQLite integrity check
  exactly. Old, future, undeclared, partial, or damaged files fail closed. The
  session store has no migration, repair, downgrade, or production in-memory
  fallback. In-memory SQLite is only a full-schema test fixture.

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
tool capability denial, approval/AskUser gates, workspace executors, memory,
MCP, Skills, compression, cancellation, and durable session restore. Real
provider tests are explicitly ignored unless credentials are supplied.

```bash
cargo test -p sylvander-agent --all-targets --locked
cargo clippy -p sylvander-agent --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p sylvander-agent --no-deps --locked
```

## Related documentation

- [`workspace-execution.md`](workspace-execution.md) — executor and coding tool
  rules.
- [`mcp.md`](mcp.md) — MCP lifecycle and bounded result handling.
- [`skills.md`](skills.md) — Skill discovery and per-turn budget.
- [`approval.md`](approval.md) — stable-identity persistent approval keys,
  invalidation, and durable-store operations.
- [`turn-context.md`](turn-context.md) — typed precedence, relevance retrieval,
  provenance, and prompt budgets.
- [`../../docs/sylvander-agent-platform.md`](../../docs/sylvander-agent-platform.md)
  — Runtime-to-Agent architecture and product scope.
