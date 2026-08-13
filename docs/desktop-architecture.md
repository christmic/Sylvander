# Sylvander desktop architecture

> Status: accepted foundation decision
>
> Updated: 2026-08-13

## Decision

Sylvander Desktop uses a Tauri 2 shell, a Svelte 5 TypeScript presentation
layer, and the existing authenticated JSON-over-WebSocket UI protocol.

The desktop application is a presentation client. Runtime remains the sole
owner of authenticated Sessions, Agent execution, provider credentials,
authorization, storage, tools, and observability.

```text
Svelte presentation and interaction state
                |
        bounded desktop gateway
                |
      Tauri WebSocket capability
                |
  sylvander-channel-ws / public UI protocol
                |
       Runtime ChannelHost boundary
```

## Evidence

### Existing Sylvander boundaries

| Evidence | Confirmed behavior |
|---|---|
| `docs/product-module-architecture.md` | Presentation clients depend on the service edge and public API; Runtime is the application composition root. |
| `sylvander-api/src/ui.rs` | The public contract already covers protocol negotiation, Sessions, history, streaming, tools, approvals, questions, plans, tasks, runtime information, context, and coding-session review. |
| `sylvander-channel-ws/src/lib.rs` | The WebSocket adapter already provides authenticated, bounded, full-duplex JSON transport for desktop clients. |
| `sylvander-tui/src/client.rs` | The TUI proves that a thin client can negotiate the protocol and render Runtime-owned Session state without embedding Runtime. |
| `sylvander-runtime/docs/channel-supervision.md` | A separate client may provide multi-session presentation without changing Channel lifecycle ownership. |

### Current upstream evidence

The following upstream sources were inspected on 2026-08-13:

- Tauri's official overview documents a Rust shell that uses the operating
  system WebView and can produce a minimal application below 600 KiB:
  <https://v2.tauri.app/start/>.
- Tauri's official architecture identifies TAO for windows and WRY for WebView
  integration: <https://github.com/tauri-apps/tauri/blob/dev/ARCHITECTURE.md>.
- Tauri's official capability system denies potentially dangerous plugin
  commands unless explicitly granted:
  <https://v2.tauri.app/security/capabilities/>.
- Tauri recommends channels for streaming Rust data to a frontend:
  <https://v2.tauri.app/develop/calling-rust/>.
- Svelte describes its compiler as moving work out of the browser and emitting
  minimal browser work: <https://svelte.dev/>.
- Kun uses one runtime for GUI and TUI and makes plans, approvals, background
  tasks, diffs, and verification visible in one workbench:
  <https://github.com/KunAgent/Kun>.
- ModelStudio OpenWork separates desktop Session/workspace/permission
  presentation from its Agent runtime:
  <https://github.com/modelstudioai/openwork>.
- Different AI OpenWork similarly keeps filesystem mutations and Agent
  behavior server-owned while Tauri owns native shell concerns:
  <https://github.com/different-ai/openwork/blob/dev/ARCHITECTURE.md>.

These products are interaction and architecture evidence only. Their code is
not copied, and Kun's non-commercial license is not introduced into Sylvander.

## Why this stack

| Candidate | Strength | Decision |
|---|---|---|
| Tauri 2 + Svelte 5 | Small system-WebView shell, Rust-native host boundary, mature text/layout/accessibility platform, compiled reactive UI | Selected |
| Electron + React | Broad ecosystem and competitor precedent | Rejected for the foundation because bundling Chromium and Node conflicts with the resource-efficiency goal. |
| Slint | Native compiled Rust UI and lightweight rendering | Deferred: rich Markdown, diff, browser preview, accessibility, and desktop component coverage would add product risk. |
| GPUI / egui / iced | Direct Rust rendering and strong control | Deferred: the first release values complete desktop interaction and assistive semantics over owning a renderer. |

Tauri does not make Web content native widgets. Platform WebViews can render
differently, so the desktop client owns explicit typography, focus, contrast,
motion, and screenshot acceptance tests.

## Ownership rules

The Svelte layer owns only ephemeral presentation state:

- selected navigation surface and selected Session;
- Composer drafts and local focus;
- expanded transcript rows and inspector tabs;
- connection presentation and retry affordances;
- optimistic visual acknowledgement that never claims durable completion.

Runtime and the public protocol own all durable or authoritative state. The
desktop client must not open Runtime databases, execute tools, read arbitrary
workspace paths, discover credentials, or infer success from a closed stream.

The Tauri shell is restricted to window lifecycle, bounded Runtime transport,
native dialogs, notifications, and future signed updates. Every capability is
deny-by-default and scoped to the main window. Shell commands and filesystem
plugins are not enabled in the foundation.

## Transport contract

The production client connects to a configured `ws://` or `wss://` endpoint,
sends `UiClientMessage::Hello`, and waits for `UiServerMessage::Welcome` before
submitting work. The bearer lease is supplied to the native transport and is
never persisted in browser storage or rendered in diagnostics.

Inbound messages are bounded by the server's configured WebSocket limit.
Streaming deltas are coalesced to at most one presentation update per animation
frame. Terminal `Done`, `Error`, or `TurnInterrupted` events settle a turn;
disconnect never implies completion.

The initial shell uses a deterministic fixture gateway so visual, responsive,
and state tests do not require credentials. Replacing it with the production
gateway must preserve the same typed store actions and must be accepted against
the compiled `sylvander-channel-ws` journey.

## Performance and quality gates

These are acceptance targets, not current claims:

- input is painted within one 16.7 ms frame under a normal streaming load;
- stream updates trigger at most one transcript render per animation frame;
- Session navigation remains responsive with 500 Sessions through windowing;
- transcript detail is progressively disclosed and long tool output is not
  mounted until expanded;
- the release JS/CSS bundle and installer size are recorded in CI;
- idle and active RSS, first-window time, and reconnect latency are recorded on
  macOS, Windows, and Linux release runners;
- keyboard-only operation, focus visibility, reduced motion, 200% zoom, and
  automated accessibility checks pass before release.

## Delivery slices

1. Build the responsive fixture-backed shell and domain store.
2. Add the bounded production WebSocket gateway and protocol conformance tests.
3. Add approval, AskUser, plan, task, diff, artifact, and settings surfaces.
4. Add native dialogs, notifications, window persistence, signing, and updates.
5. Establish cross-platform performance and accessibility release evidence.
