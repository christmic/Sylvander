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

The current `AgentLoop` builder and `AgentRun` modules still contain the old
ownership arrangement. Migration removes those fields from long-lived Agent
configuration and makes the request above the sole per-turn input. No alias for
the removed API is retained.

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
