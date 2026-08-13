# Agent execution kernel

## What this module is

The Agent execution kernel performs one bounded, provider-neutral reasoning
turn. Its public domain vocabulary is:

- `ConversationSnapshot`: exact model-visible history, without product Session
  ownership;
- `AgentExecutionContext`: trusted actor and logical execution authority
  constructed by Runtime;
- `AgentTurnRequest`: immutable model, prompt, tools, conversation, and
  authority snapshot;
- `AgentEvent`: internal chronological progress emitted while executing;
- `AgentOutcome`: updated conversation, final response, iteration count, and
  usage returned to Runtime.

## Why the boundary exists

A model loop is deterministic computation over a resolved snapshot. Product
Session ownership, persistence, client publication, credentials, and concrete
execution adapters have different failure and security semantics. Mixing them
made the previous `AgentRun` responsible for both inference and the application
service around it, which coupled Agent to Protocol, SQLite, the message bus,
MCP processes, and UI event shapes.

Separating the kernel makes three properties reviewable:

1. the same turn input produces one ordered internal event stream regardless
   of client transport;
2. no client DTO can deserialize itself into execution authority;
3. successful computation is distinct from Runtime's durable commit and
   client-visible completion.

## Construction and ownership

Runtime validates API identity, loads the durable Session revision, resolves
the exact provider/model and tool catalog, selects logical workspace bindings,
and creates `AgentTurnRequest`. The Agent may mutate a private working copy of
the conversation while executing. It returns the completed snapshot to
Runtime, which commits or rejects it as one product transaction.

The request contains immutable domain data. Runtime-selected implementations
of model access, authorization, interaction, filesystem, process execution,
and context retrieval are injected through a separate immutable execution-port
snapshot. This distinction prevents a request value from becoming a service
locator while still pinning every dependency for the duration of one turn.

`AgentLoop` now retains only stable retry, iteration, and compression policy.
Runtime owns `AgentRun` and supplies all per-turn data through
`AgentTurnRequest` plus all selected service implementations through
`AgentExecutionPorts`. No compatibility alias for the removed Agent-owned run
service is retained.

The execution boundary therefore has four conceptual layers:

1. immutable turn data (`AgentTurnRequest`);
2. immutable turn services and authority (`AgentExecutionPorts`);
3. provider-neutral execution (`AgentLoop` and `run_stream`);
4. chronological progress and terminal computation result (`AgentEvent` and
   `AgentOutcome`).

## Local source evidence

The boundary is based on inspected local source, not API-shape inference:

- Codex commit `16fbfe557446a1af94da81e1144029ccc1311ad0`,
  `codex-rs/core/src/session/turn_context.rs`: `TurnContext` freezes the model,
  provider, environment snapshot, telemetry, and per-turn configuration while
  `session/session.rs` retains thread lifecycle and services. We keep the
  per-turn snapshot idea but move product Session ownership to Runtime.
- pi commit `11b5403fade1502a9a58a9cd4e9f983a3d1d734e`,
  `packages/agent/src/agent-loop.ts` and `types.ts`: the low-level loop receives
  an explicit `AgentContext` plus `AgentLoopConfig`, clones conversation state,
  and returns newly produced messages. We keep the explicit input/output shape
  but use typed Rust ports instead of callback-heavy configuration.
- Goose commit `9d166ecee97628eced28051e7566d024f9654466`,
  `crates/goose/src/execution/manager.rs`: runtime context is supplied by the
  execution manager rather than inferred inside a tool. We keep that direction
  of dependency while requiring fail-closed prepared execution policies.

These projects are design evidence, not specifications. Sylvander's normative
contracts remain its own documented types and tests.

## Execution authority invariants

- `AgentExecutionContext` has no Serde implementation.
- Actor and logical workspace identifiers are Runtime-derived, never copied
  from tool JSON.
- The Agent sees logical workspace/target IDs, not host paths, container
  images, SSH endpoints, or credentials.
- Capabilities are explicit and empty by default.
- A process-capable tool still requires its prepared sandbox/network/filesystem
  policy to be enforced by Runtime's execution service.
- Trace identifiers correlate evidence but grant no authority.

## Documentation rules

Every module starts with `//!` documentation that explains its purpose and
ownership boundary. Every public type and field explains its semantic role and
security source. Comments explain why an invariant or ordering exists; they do
not restate Rust syntax. Imports remain at module scope.
