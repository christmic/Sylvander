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
   and Runtime. The current crate is named `sylvander-protocol`; migration will
   rename it after its Rust runtime contents have been removed.
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

### API

The eventual `sylvander-api` crate owns versioned, serializable DTOs and JSON
Schema only:

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
  -> Runtime constructs AgentTurnRequest
  -> Agent emits AgentEvent and returns AgentOutcome
  -> Runtime atomically persists history and usage
  -> Runtime maps and publishes API StreamEvent values
```

The Agent-level lifecycle remains:

```text
AgentTurnRequest
  -> build provider-neutral request
  -> model stream
  -> prepare tool call
  -> authorize through injected port
  -> validate execution environment
  -> execute and re-feed result
  -> AgentOutcome
```

## Target Agent source layout

```text
sylvander-agent/src/
  agent.rs
  conversation.rs
  request.rs
  outcome.rs
  event.rs
  error.rs
  context/
  compression/
  tool/
  ports/
```

Concrete persistence and transport implementations are intentionally absent.

## Migration order

Migration is performed without compatibility aliases:

1. Introduce Agent-owned conversation, execution-authority, request, outcome,
   and event vocabulary.
2. Make the provider/tool state machine consume that immutable request.
3. Move `AgentRun`, `AgentRunEngine`, Session persistence, and public event
   mapping into Runtime.
4. Remove Agent's Protocol dependency and move concrete infrastructure below
   Runtime-owned ports.
5. Move `MessageBus` and `InProcessMessageBus` out of Protocol; split the large
   wire `types` module by API domain.
6. Replace Channel access to Agent and `SessionStore` with `ChannelHost`.
7. Rename the pure wire crate to `sylvander-api` and add dependency-graph
   verification to CI.

At each step, the new owner becomes authoritative before the previous owner is
deleted. No deprecated alias or dual production path is retained.
