# TUI Architecture

## Goals

The TUI is organized around three non-negotiable boundaries:

1. Data is protocol-neutral and has no terminal or socket dependencies.
2. Services perform I/O and translate wire messages into domain events.
3. Presentation is read-only with respect to application state and performs no
   network, filesystem, Git, or environment access.

Configuration is resolved once at startup. Input becomes an intent before it
changes state. Side effects leave the application as `Action` values.

## Dependency direction

```text
main
  └─ config ──► theme selection + runtime metadata
  └─ runtime
       ├─ terminal_input ──► UserIntent
       ├─ application ─────► AppState + Action
       │    ├─ model
       │    └─ event
       ├─ service ─────────► DomainEvent
       │    └─ client (Unix wire adapter)
       ├─ workspace_service ► read-only local Git queries
       └─ ui
            ├─ panel
            ├─ modal
            ├─ component
            └─ theme
```

Arrows point toward dependencies. Reverse dependencies are prohibited.

## Layers

### `model.rs` — data

Contains protocol-neutral data such as `ChatMessage`, `ToolStatus`, task data,
approval tool information, and immutable runtime metadata. It imports no
Crossterm, Ratatui, Tokio, or socket type.

### `event.rs` — domain messages and effects

`DomainEvent` is input from a service. `Action` is an external effect requested
by the application. Wire structs never cross this boundary.

### `app.rs` — state and reducer

Owns the state snapshot consumed by presentation and implements deterministic
reduction. It does not own Panels. It may own interaction state such as Composer,
modal stack, live-follow position, and unread count because these affect user
interaction, but renderer instances are presentation-owned.

### `application.rs` — interaction controller

Accepts `UserIntent`, invokes the reducer or Composer, and exposes queued
`Action`s. It performs no I/O and renders nothing. This is the preferred entry
point for input tests.

`command.rs` owns command parsing, argument validation, and application-level
effects. Command palette rendering does not implement command behavior itself.

### `service.rs` and `client.rs` — service boundary

`client.rs` mirrors the Unix JSON wire format. `service.rs` hides it from the
runtime and exposes only `DomainEvent` and `Action`. A future WebSocket or replay
service must implement the same boundary without changing Panels or AppState.

Every connection starts with a transport-neutral UI protocol handshake. The
client advertises a supported version range and named capabilities; the server
selects one overlapping version and returns its capabilities before accepting
business messages. Incompatible peers fail closed with a bounded protocol
error. Negotiated truth is stored in `AppState` for status and diagnostics, not
used by Panels to infer backend behavior.

The wire reader converts malformed or unknown messages into bounded diagnostic
domain events. Raw message bodies are never copied into diagnostics or logs, so
future event types remain visible without exposing prompt or credential data.

The Unix service owns one session relay per active turn. Relays outlive an
individual socket, broadcast to every attached client, and retain an ordered,
4 MiB-bounded replay of the in-flight turn. Reattachment is atomic with relay
delivery: persisted history is sent first, then the active replay, then new live
events. Terminal turn events clear the replay and retire the relay. This keeps
recovery transport-owned without leaking socket or replay state into Panels.

`workspace_service.rs` is the corresponding boundary for bounded, read-only
local workspace queries such as `/diff`. It never mutates Git state and returns
plain domain data; Panels and Modals do not invoke it directly.

### `terminal_input.rs` — terminal adapter

Owns Crossterm event capture. Keyboard, bracketed paste, resize, and mouse wheel
events become explicit `UserIntent`s. A mouse event is never synthesized as a
keyboard arrow event.

### `ui.rs`, `panel/`, `modal/`, `component.rs`, `theme.rs` — presentation

Presentation owns the component graph and reads `&AppState`. It cannot execute
Git, read environment variables, connect to a server, or mutate application
state during render. Semantic theme roles are used instead of concrete colors.
`tool_presenter.rs` and `approval_presenter.rs` are pure semantic formatting
helpers shared by transcript and decision surfaces.

### `runtime.rs` — orchestration

Owns terminal lifecycle and Tokio scheduling. It connects input, service,
application, and presentation without absorbing their responsibilities.

## State and side-effect flow

```text
terminal event → UserIntent → Application → AppState
                                  │
                                  └→ Action → AgentService → Unix wire

Unix wire → AgentService → DomainEvent → Application → AppState → UI
```

No Panel sends service messages. No service holds a Panel. No wire message is
stored directly in presentation state.

Approval follows the same boundary: the modal selects only a server-advertised
`ApprovalScope`, `Action` and the service carry that intent, and the Agent owns
validation, session isolation, and durable storage. The TUI never caches an
approval rule or claims persistence based on local state.

Interaction deadlines follow the same ownership rule. The Agent publishes a
transport-neutral timeout kind, subject identifier, actual deadline, and
recovery class. Adapters preserve those fields unchanged. The reducer may close
the matching stale modal and explain recovery, but it never starts a local
timer, fabricates a timeout, retries a tool, or assumes that work completed.

Compaction follows the same service boundary. `/compact` produces an action;
the Agent owns session locking, model summarization, live-history replacement,
and durable-history replacement. Automatic and manual runs return the same
typed lifecycle, so presentation never infers completion from token counts.

## Adding a feature

1. Add or reuse a model type in `model.rs`.
2. Add a protocol-neutral `DomainEvent` or `Action` only if state or an external
   effect requires it.
3. Translate wire data in `client.rs`/`service.rs`.
4. Reduce state in `app.rs` or handle interaction in `application.rs`.
5. Render the state in a Panel or Modal using semantic theme functions.
6. Add reducer/input tests and a visual snapshot.

If a feature starts by importing the socket client into a Panel, or Ratatui into
`model.rs`, the design is wrong.
