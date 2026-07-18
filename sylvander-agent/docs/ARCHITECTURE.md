# `sylvander-agent` architecture

`sylvander-agent` owns the deterministic per-session Agent execution loop. It
turns an authenticated Runtime-issued session into model requests, tool calls,
stream events, durable transcript entries, and bounded background work. It is
not a server composition root and it does not expose network listeners.

## Internal layers

```text
AgentRun / AgentRunEngine
  -> prompt composition + session snapshot
  -> provider-compatible AgentLoop
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
- `tool` and `tool_context` define the invocation boundary. Tools receive
  Runtime-derived identity, workspace, capability, and execution-budget data;
  model arguments are never authority.
- `workspace_executor`, `tools`, `mcp_stdio`, and skill loading are adapters
  below the Tool boundary. They return bounded structured results and artifacts
  rather than unbounded transcript text.
- `session_store` is the durable transcript/config store. Production uses the
  SQLite implementation injected by Runtime; in-memory stores are only for
  tests or an explicit ephemeral development mode.

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
   returning to the model or a client.
5. Background tasks get a new explicit task prompt and a reduced read-only
   capability set. They do not inherit private chain-of-thought or silently
   mutate the parent session.

## Extension points

- Implement a `ModelProvider` in a provider crate, then register it in Runtime.
- Add a built-in tool through the registry and require an explicit capability,
  input schema, execution budget, and tests for both allowed and denied paths.
- Add MCP or Skill support through their dedicated loaders; never inject
  unvalidated filesystem content directly into the system prompt.
- Use `WorkspaceExecutor` for file and command work so local, container, and
  later remote targets share one result contract.

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
