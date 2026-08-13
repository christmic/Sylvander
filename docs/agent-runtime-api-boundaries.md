# Agent, Runtime, and API boundaries

This document is the target architecture for Sylvander's Agent execution,
product sessions, and client protocol. It is normative: module moves and new
features must make these dependency rules more true, not preserve the previous
mixed ownership through aliases or compatibility facades.

## Three different protocols

Sylvander has three unrelated protocol boundaries. They must not share an
implementation layer merely because all three serialize JSON.

1. `sylvander-llm-core` is the provider-neutral contract between an Agent and
   model adapters.
2. `sylvander-api` is the versioned wire contract between clients or Channels
   and Runtime.
3. Anthropic, OpenAI, and DashScope crates implement each provider's official
   wire protocol at the outer adapter edge.

The Agent participates only in the first boundary. Runtime is the bridge from
the client API to Agent inputs and from Agent events to client events.

## Ownership

### Agent

`sylvander-agent` owns how one bounded Agent execution reasons and invokes
tools:

- the provider-neutral model/tool iteration state machine;
- a conversation snapshot supplied as input and the updated snapshot returned
  as output;
- prompt composition, context budgeting, compression, retry, and cancellation
  during one execution;
- internal `AgentEvent` values;
- tool definition, preparation, registry, coordination, execution policy, and
  result normalization;
- ports for authorization, interaction, workspace access, and context
  retrieval.

The Agent does not own product sessions. It must not create, authenticate,
persist, restore, archive, route, or publish a Session. It must not know UI
messages, Channel transports, SQLite, provider credentials, MCP process
lifecycle, or concrete Local/SSH/OCI executors.

The Agent's only first-party dependency is `sylvander-llm-core`. General Rust
libraries such as Tokio, Serde, and `serde_json` remain allowed implementation
dependencies.

### Runtime

`sylvander-runtime` owns product and authority semantics:

- authenticated users, Agent revisions, and product Session lifecycle;
- durable transcript/configuration storage and optimistic Session revisions;
- selecting the exact provider, model, workspace, tools, and execution
  adapters for a turn;
- constructing non-serializable Agent execution authority;
- approval policy, durable authorization audit, and interactive decisions;
- scheduling, interruption, restart restoration, and background ownership;
- translating `AgentEvent` to client `StreamEvent`;
- concrete SQLite, MCP, Local, SSH, OCI, and other infrastructure adapters.

Runtime may depend on Agent, API, provider adapters, and infrastructure. No
lower layer may depend back on Runtime.

Runtime also owns the single configured storage facade, the built-in
observability pipeline, and the execution service. Their full product-level
contract is defined in
[`product-module-architecture.md`](product-module-architecture.md).
That document distinguishes target contracts from implemented slices; the
current storage facade and observability paths are not yet the complete target.

### API

The `sylvander-api` crate owns versioned, serializable DTOs and JSON Schema
only:

- request, response, event, identifier, and redacted view shapes;
- protocol negotiation and pure validation;
- UI, administration, identity binding, profile, and memory-confirmation
  contracts.

It must not contain Tokio channels, async traits, message-bus implementations,
database access, network clients, Agent execution, or provider adapters.

### Channels

Channels translate their native transport to the API and call a Runtime-owned
`ChannelHost` application port. A Channel must not access `SessionStore`,
`AgentRun`, `AgentRunEngine`, or other Agent internals.

## Session vocabulary

The unqualified name `SessionContext` is retired because it currently combines
product identity, wire data, and execution authority.

| Concept | Owner | Meaning |
|---|---|---|
| `SessionRecord` | Runtime | Durable product Session, owner, revision, state, and effective configuration. |
| `TurnRequest` | API/Runtime | One authenticated request to append user input to a Session. |
| `ConversationSnapshot` | Agent | Model-visible messages for one execution; no product lifecycle authority. |
| `AgentExecutionContext` | Agent value constructed by Runtime | Trusted actor, workspace, capabilities, deadline, and opaque correlation data. It is not a wire DTO. |
| `AgentTurnRequest` | Agent | Complete immutable input for one Agent execution. |
| `AgentOutcome` | Agent | Updated conversation, final response, and usage returned to Runtime. |

The Agent may maintain conversation messages while it runs. Runtime owns when
that conversation becomes durable Session history.

The Agent is trusted control-plane code that uses Runtime's sandboxed execution
service. It is not placed inside each tool sandbox. Whole-worker isolation is a
separate deployment hardening layer and never replaces per-tool policy.

## Target dependency graph

```text
Clients / TUI / Channels
          |
          v
    sylvander-api
          ^
          |
   sylvander-runtime -----> provider adapters / infrastructure
          |
          v
   sylvander-agent
          |
          v
 sylvander-llm-core
```

Required negative dependencies:

- Agent must not depend on API/Protocol, Runtime, Channels, or provider crates.
- API must not depend on Agent, Runtime, Tokio, or provider crates.
- Channels must not depend on Agent.
- Provider crates must not depend on Agent or API.
- Runtime is the only production crate allowed to depend on both Agent and
  API.

## Turn lifecycle

The product-level lifecycle is:

```text
Channel/API request
  -> Runtime authentication and Session authorization
  -> Runtime loads the pinned Session snapshot
  -> Runtime resolves model, tools, context, and execution adapters
  -> Runtime atomically commits user input + immutable config + running turn
  -> Runtime constructs AgentTurnRequest
  -> Agent emits AgentEvent and returns AgentOutcome
  -> Runtime atomically commits assistant output + completed turn terminal
  -> Runtime maps and publishes API StreamEvent values
```

An interrupted or failed execution commits the corresponding content-free
turn terminal before Runtime publishes it when the durable turn has started.
Provider-iteration usage remains a separate bounded transaction today; the
larger cross-repository transaction described in the product architecture is
still future work.

The Agent-level lifecycle remains:

```text
AgentTurnRequest + immutable AgentExecutionPorts snapshot
  -> build provider-neutral request
  -> model stream
  -> prepare tool call
  -> authorize through injected port
  -> validate execution environment
  -> execute and re-feed result
  -> AgentOutcome
```

## Current Agent source layers

```text
sylvander-agent/src/
  request.rs + conversation.rs + execution_context.rs   immutable turn data
  execution_ports.rs                                    selected turn services
  loop_.rs + compress/                                   execution kernel
  prompt.rs + turn_context.rs                            context composition
  tool.rs + tool_context.rs + tool_invocation.rs         tool policy boundary
  tools/                                                  built-in definitions
  workspace_executor.rs + artifact.rs + *_gate.rs        neutral Runtime ports
  event.rs + outcome.rs + error.rs                       progress and results
```

Concrete persistence and transport implementations are intentionally absent.

`AgentTurnRequest` carries immutable domain input. `AgentExecutionPorts`
carries Runtime-selected implementations needed to perform that input. Both
are frozen for one execution, but they remain distinct so request construction
does not become dependency lookup and ports cannot be serialized as client
authority.

## Local implementation evidence for Session and Runtime

The Session migration is based on pinned local source, not inferred product
behavior:

| Project | Commit | Reviewed implementation | Relevant result |
|---|---|---|---|
| Codex | `16fbfe557446a1af94da81e1144029ccc1311ad0` | `codex-rs/app-server/src/thread_state.rs`, `codex-rs/state/src/lib.rs`, `codex-rs/thread-store/src/queue_store.rs` | Live connection/subscriber state belongs to the application server; durable thread state has a dedicated Runtime facade; storage-neutral queue ports sit above SQLite. |
| pi coding agent | `11b5403fade1502a9a58a9cd4e9f983a3d1d734e` | `packages/coding-agent/src/core/agent-session-runtime.ts`, `agent-session.ts`, `session-manager.ts` | A session Runtime owns replace/teardown/rebind of cwd-bound services; Agent events are consumed by the session layer, which later persists finalized messages. |
| Goose | `9d166ecee97628eced28051e7566d024f9654466` | `crates/goose/src/session/session_manager.rs`, `execution/manager.rs` | Session metadata, conversation persistence, archive/list/search, and storage are grouped under a Session manager rather than the model loop. |

These implementations agree on the ownership direction but are not copied as
an API. Sylvander adopts Codex's stronger separation between live application
state and durable state, pi's explicit teardown/rebind lifecycle, and Goose's
cohesive Session operations. Sylvander does not adopt process-global Session
singletons, provider-shaped persisted messages, or a Session object that owns
the Agent kernel. Runtime owns the Session and invokes a replaceable Agent
execution with frozen inputs.

## Migration order

Migration is performed without compatibility aliases:

1. **Complete:** introduce Agent-owned conversation, execution-authority,
   request, outcome, and event vocabulary.
2. **Complete:** make the provider/tool state machine consume immutable
   `AgentTurnRequest` and `AgentExecutionPorts`. `AgentLoop` now contains only
   stable retry, iteration, and compression policy.
3. **Complete:** move `AgentRun`, `AgentRunEngine`, Session persistence, and
   public event mapping into Runtime.
4. **Complete:** remove Agent's Protocol dependency and move concrete
   infrastructure below Runtime-owned ports. MCP stdio is Runtime-owned;
   durable SQLite relationship memory, integrity anchors, backup, and
   maintenance are Runtime-owned. The host-local, SSH, and OCI workspace
   executors are Runtime-owned and Agent defaults to an unavailable executor
   until Runtime binds one. Workspace mutation journaling is now an Agent
   two-phase port with Runtime-owned manifests, crash recovery, and rollback;
   oversized tool-result persistence is likewise an Agent port with a
   Runtime-owned, explicitly rooted filesystem adapter. Prompt evidence,
   execution identity, User Profile snapshots, workspace capabilities, and
   Agent-definition values now have Agent- or Runtime-owned domain types with
   explicit Runtime projections at the API edge. Agent's normal dependency
   graph contains only `sylvander-llm-core` among first-party crates.
5. **Complete:** split the large wire `types` module by API domain.
   `MessageBus`, subscription policy, delivery errors, diagnostics, and the
   bounded `InProcessMessageBus` have moved to `sylvander-channel`; Protocol no
   longer depends on Tokio or async-trait. Runtime and Channel implementations
   consume the application port while message payloads remain Protocol DTOs.
   Message/event envelopes, public identities, evidence-bound feedback,
   Session/config, model catalog, platform inspection, execution results, and
   negotiation now have dedicated modules with their tests. The old `types`
   catch-all module is deleted; crate-root DTO names and wire/schema shapes
   remain unchanged.
6. **Complete:** replace Channel access to Agent and `SessionStore` with
   `ChannelHost`.
7. **Complete:** the executable dependency/source gate verifies Agent, API,
   Channel, provider, and Runtime boundaries and runs from the security gate.
   The pure wire crate is named `sylvander-api`; the old crate and compatibility
   aliases do not exist.

At each step, the new owner becomes authoritative before the previous owner is
deleted. No deprecated alias or dual production path is retained.
