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
