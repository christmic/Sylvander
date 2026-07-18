# Module Reference — `sylvander-protocol`

> Wire-format protocol types for Sylvander's message bus.
> Source: [`sylvander-protocol/src/`](../sylvander-protocol/src)

## 1. Purpose

`sylvander-protocol` owns Sylvander's **public, UI-facing language-neutral
contract**. Current wire DTOs derive `serde::Serialize`,
`serde::Deserialize`, and `schemars::JsonSchema`; their JSON Schema output is
the basis for TypeScript, Python, Swift, and other client code generation.
The crate also contains Rust-only in-process bus primitives, which are not
part of the generated client contract.

The crate is split into two layers:

- **Cross-language data definitions** (`types`, `boundary`, `identity_binding`,
  `user_profile`, `ui`, `schema`, `agent_admin`, `registry_admin`) — strict
  current-contract DTOs with `JsonSchema` derives.
- **Rust-only runtime types** (`bus_trait`, `in_process`, `session_context`) —
  the in-process bus and the umbrella request context.

## 2. Public surface (top-level types)

```text
mod agent_admin;       // AgentAdminRequest / AgentAdminResponse / AgentAdminError
mod boundary;          // BoundaryContext, AuthenticatedPrincipal, BoundaryError
mod bus_trait;         // MessageBus trait, SubscriptionFilter, BusDiagnostics
mod identity_binding;  // IdentityBindingRequest / IdentityBindingResponse
mod in_process;        // InProcessMessageBus (tokio mpsc-backed MessageBus)
mod registry_admin;    // RegistryAdminRequest / RegistryAdminResponse
mod schema;            // Generated JSON Schema helpers
mod session_context;   // SessionContext, Identity, Origin, RequestMeta
mod types;             // StreamEvent, BusMessage, AgentId, SessionId, UserId, ...
mod ui;                // UiClientMessage, UiServerMessage
mod user_profile;      // UserProfileRequest / UserProfileResponse
```

Re-exports from `lib.rs`:

```rust
pub use agent_admin::*;
pub use boundary::*;
pub use bus_trait::{BusDiagnostics, BusError, MessageBus, SubscriptionFilter};
pub use identity_binding::*;
pub use in_process::InProcessMessageBus;
pub use registry_admin::*;
pub use session_context::*;
pub use types::*;
pub use ui::*;
pub use user_profile::*;
```

Selected verbatim signatures:

```rust
// types.rs
pub const UI_PROTOCOL_VERSION: u16 = 4;
pub const UI_PROTOCOL_MIN_VERSION: u16 = UI_PROTOCOL_VERSION;
pub const UI_PROTOCOL_MAX_VERSION: u16 = UI_PROTOCOL_VERSION;

pub enum StreamEvent { TextDelta{delta}, ThinkingDelta{delta}, ModelRetry{...},
    ToolCall{call_id, tool_name, input}, ToolOutputDelta{...}, ToolResult{...},
    IterationStart{iteration}, IterationEnd{...}, Done{text},
    ToolApprovalRequired{batch_id, tools, allowed_scopes}, AskUser{...},
    UserAnswer{...}, TurnInterrupted{reason}, PlanProposed{...},
    TaskStarted{...}, TaskProgress{...}, TaskCompleted{...},
    CompactionStarted{automatic}, CompactionCompleted{report}, ... }

pub struct BusMessage { session_id, sender, recipient, kind, payload,
                        attachments, timestamp, id }

pub struct AgentId(pub String);
pub struct SessionId(pub String);
pub struct UserId(pub String);            // with UserId::system() sentinel

// bus_trait.rs
#[async_trait]
pub trait MessageBus: Send + Sync {
    async fn publish(&self, msg: BusMessage) -> Result<(), BusError>;
    async fn subscribe(&self, filter: SubscriptionFilter)
        -> Result<mpsc::Receiver<BusMessage>, BusError>;
    async fn diagnostics(&self) -> BusDiagnostics { ... }
}
```

## 3. Architecture

```text
             cross-language wire types          Rust runtime
            +-----------------------------+  +-------------------+
            | types                       |  | bus_trait         |
            | boundary                    |  | in_process        |
            | identity_binding            |  | session_context   |
            | user_profile                |  +-------------------+
            | ui                          |           |
            | agent_admin                 |           v
            | registry_admin              |    MessageBus trait
            | schema (JSON Schema gen)    |    InProcessMessageBus
            +-----------------------------+
                       ^                          ^
                       |                          |
                       |                          |
                 transports, agents           Sylvander runtime
                 (channels, TUI,              (agents, services)
                  CLI clients)
```

## 4. Lifecycle / data flow

A typical request crosses these layers in order:

1. **Ingress authentication.** A transport (channel) produces a
   `BoundaryContext` containing the `AuthenticatedPrincipal` (or
   `None` for unauthenticated requests), `channel_instance_id`,
   `transport`, and `request_id`.
2. **UI dispatch.** The transport parses one current-shape
   `UiClientMessage` (see `ui.rs`) and submits it with the sealed boundary to
   Runtime-owned `UiService`. Old versions, unknown fields, and unnegotiated
   operations fail before mutation.
3. **Authorized bus routing.** Runtime resolves the stable user, Agent,
   session ownership, operation policy, and optimistic revision. Only then
   does it publish the corresponding chat/control message.
   `InProcessMessageBus` matches each `BusMessage`
   against subscriber `SubscriptionFilter`s (session / recipient /
   kind) and fans out via `tokio::mpsc` channels. Backpressure is
   enforced: `publish` returns `BusError::Backpressure` if any
   matching subscriber is saturated.
4. **Runtime work.** Agents consume messages, emit `StreamEvent`s
   (through `MessageKind::Stream`), and finally write `Done{text}`.
5. **Audit/inspection.** `agent_admin` and `registry_admin` envelopes
   carry administrative operations; their responses redact secrets,
   command arguments, and workspace paths before serialization.

The `SessionContext` umbrella (`session_context.rs`) is the Rust-side
"who/where/when/why" passed into agent and tool APIs. It carries
`Identity`, `Origin`, `RequestMeta`, and a free-form `AttributeBag`
that lets new fields land without changing call-site signatures.

## 5. Configuration knobs

The crate itself does not read environment variables. Configuration
is owned by `sylvander-runtime`. Schema-generation helpers are
exposed via the example binary:

```bash
cargo run -p sylvander-protocol --example generate_ui_schema
```

Each generated schema is also available programmatically through
`crate::schema::{ui_protocol_schema, agent_admin_protocol_schema,
registry_admin_protocol_schema, identity_binding_protocol_schema,
user_profile_protocol_schema}`.

## 6. Extension rules

- Add a wire field only when every current producer and consumer can be changed
  in the same bounded change. Do not add a fallback decoder for an unspecified
  historical shape.
- Administrative mutations require a typed request, typed response, JSON
  Schema coverage, and an explicit minimum negotiated UI protocol version.
- Model selection is always the exact `(provider_id, model_id)` pair. Durable
  effective configuration requires its optimistic revision, immutable
  Agent/Provider/Model pins, and prompt manifest; missing or historical
  alternatives fail closed instead of being inferred.
- Identity and authorization values must originate at a trusted ingress.
  Client-provided display names, workspace paths, and Agent identifiers are
  requests, never proof of authority.
- New cross-language data belongs in a serde/JsonSchema module. Rust-only
  runtime helpers stay outside generated wire schemas.

## 7. Tests

| Submodule | Test file | Coverage |
|-----------|-----------|----------|
| `types` | `sylvander-protocol/tests/unit/types.rs` | Round-trip, revision pins, prompt manifest, current negotiated shapes, model selection resolution |
| `boundary` | `sylvander-protocol/tests/unit/boundary.rs` | Credentials never appear in serialized context, `AuthenticationFailure` is content-free |
| `identity_binding` | `sylvander-protocol/tests/unit/identity_binding.rs` | Secret validation, serialization redaction, one-time-secret exhaustion |
| `user_profile` | `sylvander-protocol/tests/unit/user_profile.rs` | Privacy classifications, constraint count limit, owner-free envelopes |
| `bus_trait` | exercised by `in_process/tests` | filter matching semantics |
| `in_process` | `sylvander-protocol/tests/unit/in_process.rs` | publish/subscribe, filter, backpressure rejection, concurrent publisher burst |
| `session_context` | `sylvander-protocol/tests/unit/session_context.rs` | builder methods, attribute bag, system sentinel |
| `schema` | `sylvander-protocol/tests/unit/schema.rs` | Current UI schema surface and content-safe secret handling |
| `ui` | `sylvander-protocol/tests/unit/ui.rs` | strict-shape parsing, registry/user-profile/identity-binding envelopes |
| `agent_admin` | `sylvander-protocol/tests/unit/agent_admin.rs` | conflict errors, allowed_models round-trip, prompt redaction |
| `registry_admin` | `sylvander-protocol/tests/unit/registry_admin.rs` | generation reads, credential redaction |

## 8. Related docs

- [`docs/boundary-authorization.md`](boundary-authorization.md) — boundary contract, authentication methods, denial codes.
- [`docs/identity-binding-protocol.md`](identity-binding-protocol.md) — identity link challenges, one-time secrets.
- [`docs/user-profile-protocol.md`](user-profile-protocol.md) — owner-safe profile CRUD and privacy classes.
- [`docs/sylvander-agent-platform.md`](sylvander-agent-platform.md) — Agent platform overview that consumes these types.
- [`docs/server-configuration.md`](server-configuration.md) — how runtime configuration interacts with the wire protocol.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>
